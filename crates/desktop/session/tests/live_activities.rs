//! What is live in a workspace shows on every device in it: a video Tube plays on the person's
//! laptop is in the bar of their phone, drawn by its playback session's own `view live`, and the
//! phone's bar and its lock screen's transport act on it through the session's own actions. A
//! device that goes away takes what is live on it along.
//!
//! Two real sessions of one person, each running the development session's tilolang apps, each
//! showing the tilolang shell; every step is a published action, and every check is what the
//! shell shows.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the devices live until their \
              sessions end"
)]

use serde_json::json;
use tilo_actions::remote::{CatalogueQuery, CatalogueRow, RemoteActionMessage, RemoteActionTarget};

use crate::common::people::{
    Device, Site, seed_the_apps, target, two_devices_of_one_person_with_apps, waited,
};

/// The live rows, as the session publishes them.
const LIVE: &str = "session:live";

/// The lock screen's resource, whose transport its play button runs.
const LOCK_SCREEN: &str = "lock";

/// Whether the shell on `device` shows `words`, as assistive technology reads it.
fn shows(device: &Device, words: &str) -> bool {
    let answer = device.ask(&json!({"op": "says", "text": words}));
    answer["answer"]["says"]
        .as_bool()
        .unwrap_or_else(|| panic!("{} answers `says` with {answer}", device.label))
}

/// Tube's resources, as their kinds name them (`tl.so.tilo.tube.Video`), and the playback
/// sessions it keeps in its record (`tl.so.tilo.playback.Playback`).
const TUBE_KINDS: [&str; 2] = ["tl.so.tilo.tube.", "tl.so.tilo.playback."];

/// The kind of a playback session, the record that is live while it plays.
const PLAYBACK: &str = "tl.so.tilo.playback.Playback";

/// Every row of Tube's resources `device` publishes, at every level: a video is a child of Tube's
/// root (or of a channel), never a top-level row, so the listing follows each of Tube's rows that
/// holds rows below it. The session runs the seeded apps itself, so their rows are the session's:
/// they are told apart by their kinds.
fn tube_rows(device: &Device) -> Vec<CatalogueRow> {
    let mut rows = Vec::new();
    let mut levels = vec![None::<String>];
    let mut seen = std::collections::HashSet::new();
    while let Some(parent) = levels.pop() {
        let mut query = CatalogueQuery {
            parent_resource: parent,
            max_results: tilo_actions::remote::MAX_CATALOGUE_PAGE,
            ..CatalogueQuery::default()
        };
        loop {
            let RemoteActionMessage::CatalogueResponse { result, .. } =
                device.frame(RemoteActionMessage::CatalogueRequest {
                    correlation: 1,
                    query: query.clone(),
                })
            else {
                panic!("{} answered a listing with something else", device.label);
            };
            let page = result.unwrap_or_else(|error| panic!("{} lists: {error}", device.label));
            for row in page.rows {
                if !TUBE_KINDS
                    .iter()
                    .any(|kind| row.resource.kind.starts_with(kind))
                {
                    continue;
                }
                if row.has_children && seen.insert(row.resource.id.clone()) {
                    levels.push(Some(row.resource.id.clone()));
                }
                rows.push(row);
            }
            if page.exhausted || page.next_cursor.is_empty() {
                break;
            }
            query.revision = Some(page.revision);
            query.cursor = page.next_cursor;
        }
    }
    rows
}

/// A video Tube seeded on `device`: its `action` there, and its title.
fn video_action(
    device: &Device,
    action: &str,
    title: Option<&str>,
) -> (RemoteActionTarget, String) {
    let until = tilo_test_env::hang_deadline();
    loop {
        let rows = tube_rows(device);
        if let Some(row) = rows.iter().find(|row| {
            row.resource.kind == "tl.so.tilo.tube.Video"
                && row
                    .action
                    .as_ref()
                    .is_some_and(|offered| offered.address.action == action)
                && !row.resource.label.is_empty()
                && title.is_none_or(|title| row.resource.label == title)
        }) {
            return (target(row), row.resource.label.clone());
        }
        assert!(
            std::time::Instant::now() < until,
            "{} never offered a video's {action}: Tube lists {:?}\n{}",
            device.label,
            rows.iter()
                .map(|row| format!(
                    "{} {} {:?} {}",
                    row.app_id,
                    row.resource.kind,
                    row.resource.label,
                    row.action
                        .as_ref()
                        .map_or("", |offered| offered.address.action.as_str())
                ))
                .collect::<Vec<_>>(),
            device.warnings(),
        );
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}

/// Play a video on `device`, and wait until every one of `seen_on` shows it in its bar: its title,
/// and the address of the playback session it plays in, which is what is live. Tube's `play`
/// asks the person first, since it reaches the video's service for where its bytes play from:
/// the person allows it.
fn play_on(site: &Site, device: &Device, seen_on: &[&Device]) -> (String, String) {
    let (start, title) = video_action(device, "play", None);
    let played = device.act_confirming(&start, &json!({}));
    assert!(
        played.get("err").is_none() && !played["update"].to_string().contains("Failed"),
        "{} plays {title}: {played}",
        device.label
    );
    for other in seen_on {
        waited(
            &format!(
                "{title}, playing on {}, is in {}'s bar",
                device.label, other.label
            ),
            site,
            &[device, other],
            || shows(other, &title),
        );
    }
    let session = tube_rows(device)
        .into_iter()
        .find(|row| row.resource.kind == PLAYBACK)
        .unwrap_or_else(|| panic!("{} keeps no playback session for {title}", device.label));
    (title, session.resource.id)
}

/// A video played on the laptop is in the phone's bar, and the phone's bar stops it: it then
/// plays on neither, and neither bar shows it.
#[test]
fn a_video_played_on_one_device_shows_on_the_other_and_is_stopped_from_there() {
    seed_the_apps();
    let (site, _ada, laptop, phone) = two_devices_of_one_person_with_apps();

    let (title, _) = play_on(&site, &laptop, &[&laptop, &phone]);

    // The phone's bar draws the session by its own `view live`: its Stop is the session's `stop`.
    phone.press("Stop");
    waited(
        &format!("{title}, stopped from the phone, is in neither bar"),
        &site,
        &[&laptop, &phone],
        || !shows(&laptop, &title) && !shows(&phone, &title),
    );
}

/// The lock screen's play button pauses what is live through its own type's action: the video
/// the laptop plays stands paused, still live, and the laptop's bar offers to play it on.
#[test]
fn a_video_is_paused_from_the_lock_screen() {
    seed_the_apps();
    let (site, _ada, laptop, phone) = two_devices_of_one_person_with_apps();
    let (title, _) = play_on(&site, &laptop, &[&laptop, &phone]);
    assert!(
        shows(&laptop, "Pause"),
        "a playing video's bar row offers its pause"
    );

    // What the lock screen's play button runs on the phone: the screen's own published action.
    let transport = phone
        .offered(LOCK_SCREEN, "play-pause")
        .expect("the lock screen offers its transport");
    let pressed = phone.act_wait(&transport, &json!({}));
    assert!(
        pressed.get("err").is_none(),
        "the lock screen's play button: {pressed}"
    );

    waited(
        &format!("{title} stands paused on the laptop"),
        &site,
        &[&laptop, &phone],
        || shows(&laptop, &title) && shows(&laptop, "Play") && !shows(&laptop, "Pause"),
    );
}

/// The phone keeps the laptop's video off its own bar and brings it back, through the live rows'
/// published action; then the laptop goes away, and takes what is live on it along: the phone's
/// bar no longer shows the video.
#[test]
fn an_offline_devices_live_rows_go() {
    seed_the_apps();
    let (site, _ada, mut laptop, phone) = two_devices_of_one_person_with_apps();
    let (title, key) = play_on(&site, &laptop, &[&phone]);

    let collapse = phone
        .offered(LIVE, "keep-off-bar")
        .expect("the live rows offer keeping one off the bar");
    for (collapsed, shown) in [(true, false), (false, true)] {
        let kept = phone.act_wait(&collapse, &json!({"key": key, "off": collapsed}));
        assert!(
            kept.get("err").is_none(),
            "{title} kept off the bar: {kept}"
        );
        waited(
            &format!("{title} in the phone's bar: {shown}"),
            &site,
            &[&laptop, &phone],
            || shows(&phone, &title) == shown,
        );
    }

    laptop.stop();
    waited(
        &format!("{title}, live on the laptop that went away, leaves the phone's bar"),
        &site,
        &[&phone],
        || !shows(&phone, &title),
    );
}
