//! A window one device of a person shares, pictured on their other device: two real sessions, each
//! with its own endpoint, its own engine and its own live session, as two machines on one network
//! run them.
//!
//! A shared window's pictures travel the workspace's live session, on a track of their own that the
//! viewing device asks the sharing device for. The live session is carried by the device hosting the
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

use tilo_server_protocol::workspaces::Role;

use crate::common::people::{
    Device, Person, Site, at, join_together, log_sessions_at, new_workspace, share_the_fleet,
    two_devices_of_one_person,
};
use crate::common::window::Client;

/// The app whose window is shared.
const NOTES: &str = "so.tilo.notes";
/// The provider of the session's own actions.
const SESSION: &str = "so.tilo.session";
/// What the devices' sessions log here: the live session, the shares, the channels whose keys seal
/// the live session and the routes between the devices say how far a share came only below `info`.
const LOGGED: &str = "info,tilo_session::live=debug,tilo_session::window_share=debug,\
                      tilo_workspace_keys=debug,tilo_network_actor=debug,tilo_session_client=warn";
/// How many kinds of line a failed step shows of one part of a session's log.
const SHOWN: usize = 90;
/// How many lines a failed step shows of what a session said in order.
const IN_ORDER: usize = 60;
/// How long a live session may stand as not open while a share runs before the step fails: a live session that
/// ended is settled again at once, and one its relay refuses is never let in by waiting.
const NOT_OPEN: Duration = Duration::from_secs(12);
/// How often a shared window is painted anew while a step watches its share.
const REPAINT: Duration = Duration::from_millis(200);

/// The share is renewed in a fraction of a second rather than at the production periods: what a
/// step that watches it "stay live" proves is that the device sharing it kept streaming past the
/// expiry of the viewer's last ask, and that the viewer asked again before that — which a short
/// period shows in a second as well as production's seconds do. The periods reach the sessions
/// the way production sets them (`TILO_WINDOW_SHARE_VIEW_AGAIN_MS`,
/// `TILO_WINDOW_SHARE_VIEW_EXPIRY_MS`).
fn short_view() {
    crate::common::people::share_view_periods(
        Duration::from_millis(250),
        Duration::from_millis(1000),
    );
}
/// How often the keys of a workspace three devices of one person came into may have moved by the
/// end of a test: once for each device, with room for a commit that lost a race and was made
/// again. A device that asks for the workspace's keys under two member keys has the workspace's
/// members commit one in and the other out for as long as it runs, hundreds of times a test.
const KEYS_MOVED_AT_MOST: u64 = 8;

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

/// How `device`'s session says the live session of `workspace` stands: the workspace's row in `live`
/// of the shares' reading, `Null` while the session names no live session for it.
fn live_session_of(device: &Device, workspace: &str) -> Value {
    shares(device)["live"]
        .as_array()
        .and_then(|live| {
            live.iter()
                .find(|live_session| live_session["workspace"].as_str() == Some(workspace))
                .cloned()
        })
        .unwrap_or(Value::Null)
}

/// What `device` says of who hosts `workspace` (`keeping_devices`): `Null` while it does not hold
/// it.
fn hosting_of(device: &Device, workspace: &str) -> Value {
    device.ask(&json!({"op": "keeping_devices"}))["answer"]["workspaces"]
        .as_array()
        .and_then(|held| {
            held.iter()
                .find(|row| row["workspace"].as_str() == Some(workspace))
                .cloned()
        })
        .unwrap_or(Value::Null)
}

/// What `device`'s shell holds of `workspace`: its tabs, each with what it shows, and the window
/// shares its copy of the workspace's governance names.
fn shell_of(device: &Device, workspace: &str) -> String {
    let state = device.state();
    let Some(held) = state["state"]["workspace_tree"]["workspaces"]
        .as_array()
        .and_then(|all| {
            all.iter()
                .find(|held| held["id"].as_str() == Some(workspace))
        })
    else {
        return "its shell holds no such workspace".to_owned();
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

/// The last lines of `log` that `keep` keeps, in the order they were said: a line said again at
/// once is counted and not shown again, so a key that moves and moves back reads as it happened.
fn in_order(log: &str, keep: impl Fn(&str) -> bool) -> String {
    let mut said: Vec<(usize, String, String)> = Vec::new();
    for line in log.lines().filter(|line| keep(line)) {
        let (at, text) = line.split_once(' ').unwrap_or(("", line));
        match said.last_mut() {
            Some((count, _, last)) if last == text => *count += 1,
            _ => said.push((1, at.to_owned(), text.to_owned())),
        }
    }
    let skipped = said.len().saturating_sub(IN_ORDER);
    let mut out = String::new();
    if skipped > 0 {
        let _ = writeln!(out, "({skipped} earlier lines are not shown)");
    }
    for (count, at, text) in said.iter().skip(skipped) {
        let _ = writeln!(
            out,
            "x{count} {at} {}",
            text.chars().take(420).collect::<String>()
        );
    }
    out
}

/// How far a share came on each of `devices`, for a step that never happened: what each session
/// says of its shares, of who hosts `workspace` and of who is present there, what its shell holds
/// of the workspace, and what it logged of the live session, of the workspace's channels, of the
/// shares, of the windows' control messages and of its link to the workspace's host, which is
/// what the workspace's governance and records travel.
fn report(workspace: &str, devices: &[&Device]) -> String {
    let mut out = String::new();
    for device in devices {
        let log = device.log();
        let _ = writeln!(
            out,
            "== {} (device {}) ==\nshares: {}\nhosts: {}\npresent in {workspace}: {:?}\n\
             shell in {workspace}: {}\n\
             -- the live session --\n{}\n-- the workspace's channels --\n{}\n-- the shares --\n{}\n\
             -- the windows' messages --\n{}\n-- warnings --\n{}\n\
             -- the live_session's keys and states, in order --\n{}\n\
             -- the workspace's channel frames, in order --\n{}\n\
             -- the links to the workspaces' hosts, in order --\n{}",
            device.label,
            hex::encode(device.cert.device_public_key),
            shares(device),
            hosting_of(device, workspace),
            device.present(workspace),
            shell_of(device, workspace),
            lines_of(&log, |line| line.contains("tilo_session::live")),
            lines_of(&log, |line| {
                line.contains("tilo_workspace_keys") && line.contains(workspace)
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
            in_order(&log, |line| {
                line.contains("tilo_session::live")
                    && [
                        " epoch=",
                        "not open",
                        "waits for",
                        "was not reached",
                        "refus",
                        "turned away",
                    ]
                    .iter()
                    .any(|word| line.contains(word))
            }),
            in_order(&log, |line| {
                line.contains("tilo_workspace_keys")
                    && line.contains(workspace)
                    && line.contains("group frame")
            }),
            in_order(&log, |line| {
                [
                    "native_ingress",
                    "workspace's link",
                    "tilo_peer_state::direct",
                    "tilo_peer_state::owner::session",
                ]
                .iter()
                .any(|word| line.contains(word))
            }),
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

/// Bring `workspace` to the front of `device`'s shell, so the window it opens next is a window of
/// that workspace.
fn bring_to_front(device: &Device, workspace: &str) {
    tilo_test_env::wait_until(format!("{} holds {workspace}", device.label), || {
        device.held().iter().any(|held| held == workspace)
    });
    let shown = device.act(
        &at(SESSION, "desktop:chrome", "switch-workspace"),
        &json!({"workspace": workspace}),
    );
    assert!(
        shown.get("err").is_none(),
        "{} shows {workspace}: {shown}",
        device.label
    );
    device.wait_showing(workspace);
}

/// The tab `device`'s shell holds the window titled `title` in, in `workspace`: its id, as the
/// share's action takes it, and the tile it shows.
fn tab_titled(device: &Device, workspace: &str, title: &str) -> Option<(String, String)> {
    let state = device.state();
    let tree: tilo_desktop_model::Layout =
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

/// What a shell shows in one of its tabs.
#[derive(Debug, PartialEq, Eq)]
enum Shows {
    /// A window that runs on another device, by the key of the device that hosts it.
    Hosted(String),
    /// Anything else, this device's own view of a window among them.
    Own,
    /// The shell does not hold the tab.
    Nothing,
}

/// What `device`'s shell shows in its tab `tab` of `workspace`.
fn tab_shows(device: &Device, workspace: &str, tab: &str) -> Shows {
    let state = device.state();
    let Ok(tree) = serde_json::from_value::<tilo_desktop_model::Layout>(
        state["state"]["workspace_tree"].clone(),
    ) else {
        return Shows::Nothing;
    };
    tree.workspaces
        .iter()
        .filter(|held| held.id().to_string() == workspace)
        .flat_map(|held| &held.strips)
        .flat_map(|strip| &strip.window_groups)
        .flat_map(|group| &group.tabs)
        .find(|held| held.id.to_string() == tab)
        .map_or(Shows::Nothing, |held| match &held.tile {
            tilo_desktop_model::TileContent::HostedWindow { host, .. } => {
                Shows::Hosted(host.clone())
            }
            _ => Shows::Own,
        })
}

/// Whether `device`'s copy of `workspace`'s governance names the window of the tab `tab` as shared:
/// a grant on the window's own node, as its shell took it from the governance its engine admitted.
fn governance_shares(device: &Device, workspace: &str, tab: &str) -> bool {
    let state = device.state();
    let Ok(tree) = serde_json::from_value::<tilo_desktop_model::Layout>(
        state["state"]["workspace_tree"].clone(),
    ) else {
        return false;
    };
    tree.workspaces
        .iter()
        .filter(|held| held.id().to_string() == workspace)
        .flat_map(|held| &held.policy.grants)
        .filter_map(|grant| tilo_desktop_model::window_share::window_of_node(&grant.node))
        .any(|(_, window)| window.to_string() == tab)
}

/// The tab `viewer`'s shell pictures the shared window of the tab `tab` in, as its share's reading
/// names it: empty while it names none.
fn shown_in(viewer: &Device, tab: &str) -> String {
    row_of(&shares(viewer), "watching", tab)
        .and_then(|row| row["shown_in"].as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// The epoch `device`'s session last said it seals the live session of `workspace` under: what it
/// logs when the live session opens and whenever its keys move.
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
/// live session.
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

/// Wait until both devices seal the live session of `workspace` under the same keys and have for a
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
    let before = maker.on_shell();
    let made = maker.act_wait(&new_workspace(), &json!({}));
    assert_eq!(
        made["update"]["state"],
        json!("succeeded"),
        "{} makes a workspace: {made}\n{}",
        maker.label,
        maker.diagnostic()
    );
    // The shell takes in the person's other workspaces too, as their list brings them from their
    // other devices, and one of those may land beside the one just made: the one made here is the
    // new one this device hosts.
    let made_here = || {
        maker.on_shell().into_iter().find(|workspace| {
            !before.contains(workspace)
                && hosting_of(maker, workspace)["hosted_here"] == json!(true)
        })
    };
    tilo_test_env::wait_until(
        format!("{}'s shell holds the workspace it made", maker.label),
        || made_here().is_some(),
    );
    let made = made_here().expect("the workspace the device made");
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
    let _shared = the_window_shared(workspace, sharer, viewer, title);
}

/// [`the_shared_window_arrives`], handing back the window, which is up and shared for as long as
/// it lives, and its tab on `sharer`'s shell.
fn the_window_shared(
    workspace: &str,
    sharer: &Device,
    viewer: &Device,
    title: &str,
) -> (Client, String) {
    let both = [sharer, viewer];
    bring_to_front(sharer, workspace);
    bring_to_front(viewer, workspace);
    // The pictures travel the workspace's live session: each device is in it and says so, and both
    // run through the one device that relays the workspace.
    for device in both {
        eventually(
            &format!(
                "{} says the live session of {workspace} is open",
                device.label
            ),
            Duration::from_secs(60),
            workspace,
            &both,
            || {
                let live_session = live_session_of(device, workspace);
                live_session["state"] == json!("open") && live_session["waiting_for"] == json!("")
            },
        );
    }
    assert_eq!(
        live_session_of(sharer, workspace)["relay"],
        live_session_of(viewer, workspace)["relay"],
        "both devices' live sessions run through one relay\n{}",
        report(workspace, &both)
    );
    let window = sharer.window(NOTES, title);
    eventually(
        &format!("{}'s shell holds the window's tab", sharer.label),
        Duration::from_secs(30),
        workspace,
        &both,
        || tab_titled(sharer, workspace, title).is_some(),
    );
    let (tab, shows) = tab_titled(sharer, workspace, title).expect("the window's tab");
    // The shell's record carries the window to the other device, which lists it under the same id
    // as its own view of it: a window it opens itself, and no picture of the one that runs on the
    // sharing device. The share is asked for whether that view is there yet or not, and the steps
    // below say which of the two they found.
    let listed_before = {
        let until = Instant::now() + Duration::from_secs(20);
        loop {
            if tab_shows(viewer, workspace, &tab) == Shows::Own {
                break true;
            }
            if Instant::now() >= until {
                break false;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    };
    share_tab(sharer, &tab, &shows);
    the_share_arrives(workspace, sharer, viewer, &tab, &shows, Some(listed_before));
    (window, tab)
}

/// `sharer` shares the window its shell holds in `tab`, which shows `shows`.
fn share_tab(sharer: &Device, tab: &str, shows: &str) {
    let asked = sharer.act(
        &at(SESSION, "session:window-share", "share"),
        &json!({"tab": tab}),
    );
    assert!(
        asked.get("err").is_none(),
        "{} shares tab {tab} ({shows}): {asked}",
        sharer.label
    );
}

/// The share of `tab` that `sharer` asked for reaches `viewer`: the steps after the asking, in the
/// order a share takes them. `listed_before` is whether `viewer`'s shell listed the tab as its own
/// view of the window before the share was asked for, where the caller looked.
fn the_share_arrives(
    workspace: &str,
    sharer: &Device,
    viewer: &Device,
    tab: &str,
    shows: &str,
    listed_before: Option<bool>,
) {
    let both = [sharer, viewer];
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
            row_of(&shares(sharer), "sharing", tab)
                .is_some_and(|row| row["waiting_for"] == json!(""))
        },
    );
    // The share is the workspace's: it left the sharing device with the rest of what that device
    // writes into the workspace's governance, and the other device's copy of it names the window.
    eventually(
        &format!(
            "{} reads in its copy of the workspace's governance that {} shares tab {tab}",
            viewer.label, sharer.label
        ),
        Duration::from_secs(40),
        workspace,
        &both,
        || governance_shares(viewer, workspace, tab),
    );
    // It reads the share there and asks the device the window runs on for it.
    eventually(
        &format!("{} watches tab {tab} of {}", viewer.label, sharer.label),
        Duration::from_secs(40),
        workspace,
        &both,
        || {
            row_of(&shares(viewer), "watching", tab).is_some_and(|row| {
                row["host"].as_str().is_some_and(|host| {
                    host.parse::<iroh::EndpointId>()
                        .is_ok_and(|host| *host.as_bytes() == sharer.cert.device_public_key)
                })
            })
        },
    );
    // The window's first keyframe crossed the workspace's live session.
    eventually(
        &format!(
            "{} received the window of tab {tab} that {} shares",
            viewer.label, sharer.label
        ),
        Duration::from_secs(60),
        workspace,
        &both,
        || row_of(&shares(viewer), "received", tab).is_some(),
    );
    // Its tile decodes it, and the sharing device names who views it.
    eventually(
        &format!("{}'s shell pictures tab {tab}", viewer.label),
        Duration::from_secs(20),
        workspace,
        &both,
        || {
            row_of(&shares(viewer), "received", tab)
                .is_some_and(|row| row["pictured"] == json!(true))
        },
    );
    // What it pictures is the window that runs on the sharing device, in a tab that names that
    // device: a tab of its own, beside the shell's own view of the window where it lists one.
    let pictured_in = || {
        row_of(&shares(viewer), "watching", tab)
            .and_then(|row| row["shown_in"].as_str().map(str::to_owned))
            .unwrap_or_default()
    };
    let sharing_device = hex::encode(sharer.cert.device_public_key);
    eventually(
        &format!(
            "{}'s shell holds the window of tab {tab} in a tab that names {} as the device it \
             runs on",
            viewer.label, sharer.label
        ),
        Duration::from_secs(20),
        workspace,
        &both,
        || {
            matches!(
                tab_shows(viewer, workspace, &pictured_in()),
                Shows::Hosted(host) if host.eq_ignore_ascii_case(&sharing_device)
            )
        },
    );
    let place = pictured_in();
    let own_view = tab_shows(viewer, workspace, tab) == Shows::Own;
    println!(
        "{} listed tab {tab} as its own view of the window before the share: {listed_before:?}; \
         it lists it so now: {own_view}; it pictures the shared window in tab {place}",
        viewer.label
    );
    if own_view {
        assert_ne!(
            place,
            tab,
            "the shared window stands in a tab of its own beside the shell's own view of it\n{}",
            report(workspace, &both)
        );
    }
    eventually(
        &format!(
            "{} names who views tab {tab}, and says it sends the window",
            sharer.label
        ),
        Duration::from_secs(20),
        workspace,
        &both,
        || {
            row_of(&shares(sharer), "sharing", tab).is_some_and(|row| {
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
    short_view();
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
    short_view();
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
/// started, the device relaying the workspace was not there to reach. Its live session opens once
/// that device is back, and what it shares the hosting device pictures.
#[test]
fn a_window_shared_after_the_host_came_back_last_is_pictured() {
    short_view();
    log_sessions_at(LOGGED);
    let (_site, _ada, mut laptop, mut phone) = two_devices_of_one_person();
    let made = made_on(&phone, &laptop);
    // Neither device is behind on the workspace's keys when it goes off: a change of them that
    // reached the laptop only after the phone came back would have its live session settled again, and
    // hide that nothing else does.
    keys_at_rest(&made, &laptop, &phone);
    laptop.stop();
    phone.stop();
    let missed = missed_relays(&laptop, &made);
    laptop.start();
    laptop.wait_holding(&made);
    eventually(
        "the laptop asked the phone, which is off, for the workspace's live session",
        Duration::from_secs(60),
        &made,
        &[&laptop],
        || missed_relays(&laptop, &made) > missed,
    );
    phone.start();
    phone.wait_holding(&made);
    the_shared_window_arrives(&made, &laptop, &phone, "Shared before the host was back");
}

/// A desktop that hosts the workspace its shell made, and a machine copied from it while it was
/// off, re-keyed at boot and certified by their person: the copy holds the workspace from the
/// copied disk and its host takes it in as the person's further device. The workspace's id.
fn a_desktop_and_its_copy() -> (Person, Device, Device, String) {
    let ada = Person::new("Ada");
    let mut desktop = ada.device("Ada's desktop");
    desktop.start();
    tilo_test_env::wait_until("the desktop shows the workspace its shell made", || {
        desktop.showing().is_some()
    });
    let workspace = desktop.showing().expect("the workspace the shell made");
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
    short_view();
    log_sessions_at(LOGGED);
    let (_ada, desktop, copy, workspace) = a_desktop_and_its_copy();
    the_shared_window_arrives(&workspace, &copy, &desktop, "Shared by the copy");
}

/// One share that runs: the tab of the window, the device sharing it and the device picturing it,
/// in `workspace`.
struct Running<'a> {
    workspace: &'a str,
    tab: &'a str,
    sharer: &'a Device,
    viewer: &'a Device,
}

/// How the live session of one workspace stands on each of some devices, read again and again: the
/// course says each change of it, which is what a step that fails shows of the way there.
struct LiveSessions<'a> {
    workspace: &'a str,
    devices: Vec<&'a Device>,
    began: Instant,
    stood: BTreeMap<String, String>,
    closed: BTreeMap<String, Instant>,
    course: Vec<String>,
}

impl<'a> LiveSessions<'a> {
    fn of(workspace: &'a str, devices: &[&'a Device]) -> Self {
        Self {
            workspace,
            devices: devices.to_vec(),
            began: Instant::now(),
            stood: BTreeMap::new(),
            closed: BTreeMap::new(),
            course: Vec::new(),
        }
    }

    /// Read every device's live session once. A live session its relay refused, or one that is not open for
    /// longer than [`NOT_OPEN`], is why the workspace's live session does not carry what is said in
    /// it: the error says which device's, and how it reads.
    fn read(&mut self) -> Result<(), String> {
        for device in &self.devices {
            let live_session = live_session_of(device, self.workspace);
            let state = live_session["state"]
                .as_str()
                .unwrap_or("absent")
                .to_owned();
            if self.stood.get(&device.label) != Some(&state) {
                self.course.push(format!(
                    "{:>6.1}s {}: {live_session}",
                    self.began.elapsed().as_secs_f32(),
                    device.label
                ));
                self.stood.insert(device.label.clone(), state.clone());
            }
            if state == "open" {
                self.closed.remove(&device.label);
                continue;
            }
            if state == "refused" {
                return Err(format!(
                    "{}'s live session was refused: {live_session}",
                    device.label
                ));
            }
            let since = *self
                .closed
                .entry(device.label.clone())
                .or_insert_with(Instant::now);
            if since.elapsed() > NOT_OPEN {
                return Err(format!(
                    "{}'s live session is not open for {:?}: {live_session}",
                    device.label,
                    since.elapsed()
                ));
            }
        }
        Ok(())
    }

    /// The course so far, one change a line.
    fn course(&self) -> String {
        self.course.join("\n")
    }
}

/// How many objects of other devices `device`'s live session of `workspace` opened so far.
fn heard_by(device: &Device, workspace: &str) -> u64 {
    live_session_of(device, workspace)["heard"]
        .as_u64()
        .unwrap_or(0)
}

/// Paint `window` anew in `shade`, so the device sharing it has another picture of it to send.
fn repaint(window: &mut Client, shade: u8) {
    let (width, height) = window.seen.size_at(0).unwrap_or((320, 200));
    window.show(0, width, height, [shade, 0x60, 0xa0, 0xff]);
    window.exchange();
}

/// For `during`, with nothing shared, the live session of `workspace` stays open on each of
/// `devices`.
#[track_caller]
fn the_live_sessions_stay_open(
    after: &str,
    during: Duration,
    workspace: &str,
    devices: &[&Device],
) {
    let mut live_sessions = LiveSessions::of(workspace, devices);
    let began = Instant::now();
    while began.elapsed() < during {
        if let Err(why) = live_sessions.read() {
            panic!(
                "after {after}: {why}\nthe live sessions' course:\n{}\n{}",
                live_sessions.course(),
                report(workspace, devices)
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// For `during`, the share `share` stays live while its `window` keeps changing: the live session of its
/// workspace stays open on the sharing device, on the viewing device and on each device `beside`
/// them, the viewing device keeps hearing through both halves of the time, and at its end the
/// sharing device still says it sends the window and waits for nothing, and the viewing device
/// still pictures it. `after` names what happened before, which is what a failure blames.
#[track_caller]
fn the_share_stays_live(
    after: &str,
    during: Duration,
    share: &Running<'_>,
    window: &mut Client,
    beside: &[&Device],
) {
    let mut devices = vec![share.sharer, share.viewer];
    devices.extend_from_slice(beside);
    let mut live_sessions = LiveSessions::of(share.workspace, &devices);
    let began = Instant::now();
    let mut heard = vec![heard_by(share.viewer, share.workspace)];
    let mut tint = 0_u8;
    let mut painted: Option<Instant> = None;
    while began.elapsed() < during {
        if painted.is_none_or(|at| at.elapsed() >= REPAINT) {
            tint = tint.wrapping_add(37);
            repaint(window, tint);
            painted = Some(Instant::now());
        }
        window.exchange();
        if let Err(why) = live_sessions.read() {
            panic!(
                "after {after}: {why}\nthe live sessions' course:\n{}\n{}",
                live_sessions.course(),
                report(share.workspace, &devices)
            );
        }
        if heard.len() == 1 && began.elapsed() >= during / 2 {
            heard.push(heard_by(share.viewer, share.workspace));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    heard.push(heard_by(share.viewer, share.workspace));
    let sharing = row_of(&shares(share.sharer), "sharing", share.tab);
    let received = row_of(&shares(share.viewer), "received", share.tab);
    let sends = sharing
        .as_ref()
        .is_some_and(|row| row["streaming"] == json!(true) && row["waiting_for"] == json!(""));
    let pictured = received
        .as_ref()
        .is_some_and(|row| row["pictured"] == json!(true));
    let kept_hearing = heard.windows(2).all(|pair| pair[1] > pair[0]);
    assert!(
        sends && pictured && kept_hearing,
        "after {after}, the share of tab {} did not stay live for {during:?}: {} says of it \
         {sharing:?}, {} received {received:?} and heard {heard:?} objects at the start, half way \
         and at the end\nthe live_sessions' course:\n{}\n{}",
        share.tab,
        share.sharer.label,
        share.viewer.label,
        live_sessions.course(),
        report(share.workspace, &devices)
    );
}

/// The sharing device stops the share `share`, and the viewing device holds nothing of it any more.
fn the_share_stops(share: &Running<'_>) {
    let both = [share.sharer, share.viewer];
    let asked = share.sharer.act(
        &at(SESSION, "session:window-share", "stop"),
        &json!({"tab": share.tab}),
    );
    assert!(
        asked.get("err").is_none(),
        "{} stops sharing tab {}: {asked}",
        share.sharer.label,
        share.tab
    );
    eventually(
        &format!("{} no longer shares tab {}", share.sharer.label, share.tab),
        Duration::from_secs(40),
        share.workspace,
        &both,
        || {
            let says = shares(share.sharer);
            row_of(&says, "sharing", share.tab).is_none()
                && row_of(&says, "starting", share.tab).is_none()
        },
    );
    the_share_is_gone_from(share, share.viewer);
}

/// `viewer`, a device that pictured the window of the share `share`, holds nothing of it any more
/// once its sharer stopped it.
fn the_share_is_gone_from(share: &Running<'_>, viewer: &Device) {
    eventually(
        &format!(
            "{} neither watches nor holds tab {} any more",
            viewer.label, share.tab
        ),
        Duration::from_secs(40),
        share.workspace,
        &[share.sharer, viewer],
        || {
            let says = shares(viewer);
            row_of(&says, "watching", share.tab).is_none()
                && row_of(&says, "received", share.tab).is_none()
        },
    );
}

/// `viewer`'s shell shows no window of another device in its tab `place` any more, where it
/// pictured the window of the share `share` while that ran: the shared window's tab went with the
/// share, and what the shell lists there now, if anything, is its own view of the window.
fn the_shared_windows_tab_is_gone_from(share: &Running<'_>, viewer: &Device, place: &str) {
    eventually(
        &format!(
            "{}'s shell no longer shows the window {} shared in its tab {place}",
            viewer.label, share.sharer.label
        ),
        Duration::from_secs(20),
        share.workspace,
        &[share.sharer, viewer],
        || !matches!(tab_shows(viewer, share.workspace, place), Shows::Hosted(_)),
    );
}

/// A shared window is not a moment: the person's other device shares one with the device hosting
/// the workspace, and it stays live while it changes, through the share's end and a second share
/// of the same window. On two machines the pictures arrived and then the hosting device refused
/// the sharing one from the workspace's live session for good.
#[test]
fn a_shared_window_stays_live_through_its_end_and_a_second_share() {
    short_view();
    log_sessions_at(LOGGED);
    let (_site, _ada, laptop, phone) = two_devices_of_one_person();
    let made = made_on(&phone, &laptop);
    let (mut window, tab) = the_window_shared(&made, &laptop, &phone, "Shared for a while");
    let share = Running {
        workspace: &made,
        tab: &tab,
        sharer: &laptop,
        viewer: &phone,
    };
    the_share_stays_live(
        "the window was received",
        Duration::from_secs(3),
        &share,
        &mut window,
        &[],
    );
    the_share_stops(&share);
    the_live_sessions_stay_open(
        "the share was stopped",
        Duration::from_secs(1),
        &made,
        &[&laptop, &phone],
    );
    share_tab(&laptop, &tab, "the window shared before");
    the_share_arrives(
        &made,
        &laptop,
        &phone,
        &tab,
        "the window shared before",
        None,
    );
    the_share_stays_live(
        "the window was shared a second time",
        Duration::from_secs(2),
        &share,
        &mut window,
        &[],
    );
}

/// One person with three devices, two of them in the workspace their site hosts and the third not
/// started yet: all three know the person's fleet, as devices one machine enrolled do.
fn three_devices_of_one_person() -> (Site, Person, Device, Device, Device) {
    let ada = Person::new("Ada");
    let site = Site::start(&ada, "Studio");
    let mut laptop = ada.device("Ada's laptop");
    let mut phone = ada.device("Ada's phone");
    let tablet = ada.device("Ada's tablet");
    share_the_fleet(&ada, &[&laptop, &phone, &tablet]);
    // Neither join needs the other device: both boot together, and the third is not started yet.
    let hers = site.invite(ada.public, Role::Editor);
    let hers_phone = site.invite(ada.public, Role::Editor);
    join_together(
        &mut [&mut laptop, &mut phone],
        &[hers.as_str(), hers_phone.as_str()],
    );
    for device in [&laptop, &phone] {
        device.wait_holding(&site.workspace);
    }
    (site, ada, laptop, phone, tablet)
}

/// A third device of the person comes into the workspace while a window is shared in it, and goes
/// off again: the workspace admits it, its keys move for it, and the share between the other two
/// stays live through both.
#[test]
fn a_shared_window_stays_live_while_a_third_device_of_the_person_comes_and_goes() {
    short_view();
    log_sessions_at(LOGGED);
    let (site, ada, laptop, phone, mut tablet) = three_devices_of_one_person();
    let made = made_on(&phone, &laptop);
    let (mut window, tab) = the_window_shared(&made, &laptop, &phone, "Shared while one joins");
    let share = Running {
        workspace: &made,
        tab: &tab,
        sharer: &laptop,
        viewer: &phone,
    };
    the_share_stays_live(
        "the window was received",
        Duration::from_secs(1),
        &share,
        &mut window,
        &[],
    );
    let invitation = site.invite(ada.public, Role::Editor);
    tablet.join(&invitation);
    tablet.wait_holding(&site.workspace);
    let all = [&laptop, &phone, &tablet];
    eventually(
        &format!("{} holds {made} and its host took it in", tablet.label),
        Duration::from_secs(120),
        &made,
        &all,
        || {
            tablet.held().contains(&made)
                && hosting_of(&tablet, &made)["awaiting_host"] == json!(false)
        },
    );
    eventually(
        &format!("{} says the live session of {made} is open", tablet.label),
        Duration::from_secs(60),
        &made,
        &all,
        || live_session_of(&tablet, &made)["state"] == json!("open"),
    );
    the_share_stays_live(
        "a third device of the person was admitted",
        Duration::from_secs(2),
        &share,
        &mut window,
        &[&tablet],
    );
    tablet.stop();
    the_share_stays_live(
        "the third device went off",
        Duration::from_secs(2),
        &share,
        &mut window,
        &[],
    );
    // Each device is one member of the key group of the workspace their site hosts: its keys
    // moved as the three came into it, and not on and on while they ran.
    for device in [&laptop, &phone] {
        let epoch = sealing_epoch(device, &site.workspace);
        assert!(
            epoch.is_some_and(|epoch| epoch <= KEYS_MOVED_AT_MOST),
            "{} seals the live session of {}, which three devices came into, under the keys of \
             epoch {epoch:?}: they moved more than {KEYS_MOVED_AT_MOST} times\n{}",
            device.label,
            site.workspace,
            report(&site.workspace, &[&laptop, &phone])
        );
    }
}

/// The machines the failure was found on, all three: one makes and hosts the workspace, a copy of
/// it shares a window, and a second copy, made when the first was, is started later and stopped
/// again. The share stays live through that, through its end and through a second share.
#[test]
fn a_window_a_copied_machine_shares_stays_live_while_a_second_copy_comes_and_goes() {
    short_view();
    log_sessions_at(LOGGED);
    let ada = Person::new("Ada");
    let mut desktop = ada.device("Ada's desktop");
    desktop.start();
    tilo_test_env::wait_until("the desktop shows the workspace its shell made", || {
        desktop.showing().is_some()
    });
    let workspace = desktop.showing().expect("the workspace the shell made");
    desktop.stop();
    let (mut copy, _) = ada.branch(&desktop);
    let (mut later, _) = ada.branch(&desktop);
    // Two copies of one machine are called alike, and a device logs under what it is called.
    later.label = format!("{} made later", later.label);
    share_the_fleet(&ada, &[&desktop, &copy, &later]);
    desktop.start();
    desktop.wait_holding(&workspace);
    copy.start();
    let taken_in = |device: &Device| {
        let row = hosting_of(device, &workspace);
        row["awaiting_host"] == json!(false) && row["hosted_here"] == json!(false)
    };
    eventually(
        &format!("{} holds {workspace} and its host took it in", copy.label),
        Duration::from_secs(120),
        &workspace,
        &[&desktop, &copy],
        || taken_in(&copy),
    );
    let (mut window, tab) = the_window_shared(&workspace, &copy, &desktop, "Shared by the copy");
    let share = Running {
        workspace: &workspace,
        tab: &tab,
        sharer: &copy,
        viewer: &desktop,
    };
    the_share_stays_live(
        "the window was received",
        Duration::from_secs(2),
        &share,
        &mut window,
        &[],
    );
    later.start();
    eventually(
        &format!("{} holds {workspace} and its host took it in", later.label),
        Duration::from_secs(120),
        &workspace,
        &[&desktop, &copy, &later],
        || taken_in(&later),
    );
    the_share_stays_live(
        "a second copy of the machine was admitted",
        Duration::from_secs(2),
        &share,
        &mut window,
        &[],
    );
    later.stop();
    the_share_stays_live(
        "the second copy went off",
        Duration::from_secs(1),
        &share,
        &mut window,
        &[],
    );
    the_share_stops(&share);
    the_live_sessions_stay_open(
        "the share was stopped",
        Duration::from_secs(1),
        &workspace,
        &[&desktop, &copy],
    );
    share_tab(&copy, &tab, "the window shared before");
    the_share_arrives(
        &workspace,
        &copy,
        &desktop,
        &tab,
        "the window shared before",
        None,
    );
    the_share_stays_live(
        "the window was shared a second time",
        Duration::from_secs(2),
        &share,
        &mut window,
        &[],
    );
}

/// The three machines in the order they were made and started: the desktop makes the workspace and
/// hosts it; a copy of it is taken in and stays on from then; the desktop is stopped once more and
/// copied again, as it is by then, with the first copy among the workspace's devices and its
/// channels moved on; the second copy starts while the desktop is still off, then the desktop, and
/// both start their sessions once more. Only then does the first copy share a window, with the
/// second copy on, and the share stays live while it goes off, through its end and a second share.
#[test]
fn a_window_shared_after_the_host_was_copied_again_stays_live() {
    short_view();
    log_sessions_at(LOGGED);
    let ada = Person::new("Ada");
    let mut desktop = ada.device("Ada's desktop");
    desktop.start();
    tilo_test_env::wait_until("the desktop shows the workspace its shell made", || {
        desktop.showing().is_some()
    });
    let workspace = desktop.showing().expect("the workspace the shell made");
    desktop.stop();
    let (mut copy, _) = ada.branch(&desktop);
    share_the_fleet(&ada, &[&desktop, &copy]);
    desktop.start();
    desktop.wait_holding(&workspace);
    copy.start();
    let taken_in = |device: &Device| {
        let row = hosting_of(device, &workspace);
        row["awaiting_host"] == json!(false) && row["hosted_here"] == json!(false)
    };
    eventually(
        &format!("{} holds {workspace} and its host took it in", copy.label),
        Duration::from_secs(120),
        &workspace,
        &[&desktop, &copy],
        || taken_in(&copy),
    );
    keys_at_rest(&workspace, &desktop, &copy);

    // The desktop is copied a second time, as it is now. The first copy stays on.
    desktop.stop();
    let (mut later, _) = ada.branch(&desktop);
    // Two copies of one machine are called alike but for their keys' first letters, and a device
    // logs under what it is called.
    later.label = format!("{} made later", later.label);
    share_the_fleet(&ada, &[&desktop, &copy, &later]);
    later.start();
    // The copy is up and holds the workspace before the machine it was copied from comes back: two
    // copies of one machine on the network at once, and the desktop finds a device that already
    // took its place in the workspace.
    later.wait_holding(&workspace);
    desktop.start();
    desktop.wait_holding(&workspace);
    // Both start their sessions again on the network they now share.
    desktop.stop();
    desktop.start();
    desktop.wait_holding(&workspace);
    later.stop();
    later.start();
    let all = [&desktop, &copy, &later];
    eventually(
        &format!("{} holds {workspace} and its host took it in", later.label),
        Duration::from_secs(120),
        &workspace,
        &all,
        || taken_in(&later),
    );
    for device in all {
        eventually(
            &format!(
                "{} says the live session of {workspace} is open, with three devices in it",
                device.label
            ),
            Duration::from_secs(90),
            &workspace,
            &all,
            || live_session_of(device, &workspace)["state"] == json!("open"),
        );
    }

    let (mut window, tab) = the_window_shared(&workspace, &copy, &desktop, "Shared by the copy");
    let share = Running {
        workspace: &workspace,
        tab: &tab,
        sharer: &copy,
        viewer: &desktop,
    };
    the_share_stays_live(
        "the window was received, with the second copy on",
        Duration::from_secs(3),
        &share,
        &mut window,
        &[&later],
    );
    later.stop();
    the_share_stays_live(
        "the second copy went off",
        Duration::from_secs(2),
        &share,
        &mut window,
        &[],
    );
    the_share_stops(&share);
    the_live_sessions_stay_open(
        "the share was stopped",
        Duration::from_secs(1),
        &workspace,
        &[&desktop, &copy],
    );
    share_tab(&copy, &tab, "the window shared before");
    the_share_arrives(
        &workspace,
        &copy,
        &desktop,
        &tab,
        "the window shared before",
        None,
    );
    the_share_stays_live(
        "the window was shared a second time",
        Duration::from_secs(2),
        &share,
        &mut window,
        &[],
    );
}

/// `device`, a further device of the person, comes into `workspace`, which one of their devices
/// made and hosts: it joins their site, takes the workspace from their list, the workspace's host
/// takes it in, and its live session of the workspace opens. `beside` are the workspace's devices
/// already there.
fn comes_into(
    site: &Site,
    person: &Person,
    device: &mut Device,
    workspace: &str,
    beside: &[&Device],
) {
    let invitation = site.invite(person.public, Role::Editor);
    device.join(&invitation);
    device.wait_holding(&site.workspace);
    let device = &*device;
    let mut all = beside.to_vec();
    all.push(device);
    eventually(
        &format!("{} holds {workspace} and its host took it in", device.label),
        Duration::from_secs(120),
        workspace,
        &all,
        || {
            device.held().iter().any(|held| held == workspace)
                && hosting_of(device, workspace)["awaiting_host"] == json!(false)
        },
    );
    eventually(
        &format!(
            "{} says the live session of {workspace} is open",
            device.label
        ),
        Duration::from_secs(60),
        workspace,
        &all,
        || live_session_of(device, workspace)["state"] == json!("open"),
    );
}

/// The device that made the workspace, and hosts it, shares a window in it: the person's other
/// device, which took the workspace from their list, pictures it. The share is written into the
/// governance the hosting device keeps itself, so no host carries it on: it has to leave that
/// device for the ones linked to it.
#[test]
fn a_window_the_hosting_device_shares_is_pictured_on_the_persons_other_device() {
    short_view();
    log_sessions_at(LOGGED);
    let (_site, _ada, laptop, phone) = two_devices_of_one_person();
    let made = made_on(&phone, &laptop);
    the_shared_window_arrives(&made, &phone, &laptop, "Shared by the host");
}

/// The hosting device shares a window with two other devices of the person in the workspace: each
/// of them pictures it, and the hosting device names both as viewing it.
#[test]
fn a_window_the_hosting_device_shares_is_pictured_on_each_of_two_other_devices() {
    short_view();
    log_sessions_at(LOGGED);
    let (site, ada, laptop, phone, mut tablet) = three_devices_of_one_person();
    let made = made_on(&phone, &laptop);
    comes_into(&site, &ada, &mut tablet, &made, &[&phone, &laptop]);
    bring_to_front(&tablet, &made);
    let (_window, tab) = the_window_shared(&made, &phone, &laptop, "Shared by the host with two");
    the_share_arrives(
        &made,
        &phone,
        &tablet,
        &tab,
        "the window the laptop pictures",
        None,
    );
    eventually(
        &format!("{} names two devices as viewing tab {tab}", phone.label),
        Duration::from_secs(20),
        &made,
        &[&phone, &laptop, &tablet],
        || {
            row_of(&shares(&phone), "sharing", &tab).is_some_and(|row| {
                row["viewers"]
                    .as_array()
                    .is_some_and(|viewers| viewers.len() == 2)
            })
        },
    );
}

/// Both devices were off and the hosting device came back last: the person's other device was up
/// and asking for a host that was not there when the hosting device started again. Its link to
/// the workspace is made once that device is back, and what the hosting device shares then, from
/// a session that read the workspace's governance off its disk, the other device pictures.
#[test]
fn a_window_the_hosting_device_shares_after_it_came_back_last_is_pictured() {
    short_view();
    log_sessions_at(LOGGED);
    let (_site, _ada, mut laptop, mut phone) = two_devices_of_one_person();
    let made = made_on(&phone, &laptop);
    // Neither device is behind on the workspace's keys when it goes off: a change of them that
    // reached the laptop only after the phone came back would have its live session settled again, and
    // hide that nothing else does.
    keys_at_rest(&made, &laptop, &phone);
    laptop.stop();
    phone.stop();
    let missed = missed_relays(&laptop, &made);
    laptop.start();
    laptop.wait_holding(&made);
    eventually(
        "the laptop asked the phone, which is off, for the workspace's live session",
        Duration::from_secs(60),
        &made,
        &[&laptop],
        || missed_relays(&laptop, &made) > missed,
    );
    phone.start();
    phone.wait_holding(&made);
    the_shared_window_arrives(
        &made,
        &phone,
        &laptop,
        "Shared by the host once it was back",
    );
}

/// The hosting device stops sharing a window two other devices picture, one of which came into the
/// workspace while the share ran and read it in the governance it caught up on: the share leaves
/// the workspace's governance on each of them as it came, neither holds anything of the window any
/// more, and the tab each pictured it in is gone from its shell.
#[test]
fn a_share_the_hosting_device_stops_is_shown_on_no_other_device() {
    short_view();
    log_sessions_at(LOGGED);
    let (site, ada, laptop, phone, mut tablet) = three_devices_of_one_person();
    let made = made_on(&phone, &laptop);
    let (mut window, tab) = the_window_shared(&made, &phone, &laptop, "Shared by the host a while");
    comes_into(&site, &ada, &mut tablet, &made, &[&phone, &laptop]);
    bring_to_front(&tablet, &made);
    the_share_arrives(
        &made,
        &phone,
        &tablet,
        &tab,
        "the window the laptop pictures",
        None,
    );
    let share = Running {
        workspace: &made,
        tab: &tab,
        sharer: &phone,
        viewer: &laptop,
    };
    the_share_stays_live(
        "a third device of the person came to picture the window",
        Duration::from_secs(1),
        &share,
        &mut window,
        &[&tablet],
    );
    let pictured = [
        (&laptop, shown_in(&laptop, &tab)),
        (&tablet, shown_in(&tablet, &tab)),
    ];
    the_share_stops(&share);
    the_share_is_gone_from(&share, &tablet);
    for (viewer, place) in pictured {
        eventually(
            &format!(
                "{}'s copy of the workspace's governance no longer names tab {tab} as shared",
                viewer.label
            ),
            Duration::from_secs(20),
            &made,
            &[&phone, viewer],
            || !governance_shares(viewer, &made, &tab),
        );
        the_shared_windows_tab_is_gone_from(&share, viewer, &place);
    }
    the_live_sessions_stay_open(
        "the share was stopped",
        Duration::from_secs(1),
        &made,
        &[&phone, &laptop, &tablet],
    );
}
