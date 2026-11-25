//! One workspace, one checklist, two members' devices: a hand-written Rust app keeps its state in
//! the workspace, so the item the first member adds and the title they type arrive in the window
//! open on the other member's laptop, with the typist's caret, and each sees the other's caret
//! move; both members typing in one title at once merge their words; the first member's undo takes
//! back only their own step; and the checklist's history on the second device names each edit by
//! the member who made it.
//!
//! Both devices are real sessions, joined to one workspace a site serves, each running the
//! installed Shared Checklist example app: the checklist is the workspace's record, so this is the
//! path a person takes when they add a thing on one device and look at the other.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::arithmetic_side_effects,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the devices live until their \
              sessions end"
)]

use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::common::people::{Device, SHARED_CHECKLIST, show, two_people_at_a_site_carrying};

/// The item both devices read, and what the first member types after it.
const ADDED: &str = "Buy milk";
/// What the first member types into the item's title, after those words arrive.
const TYPED: &str = " and bread";
/// What each member types while both hold the same title, at their own carets: one at its start,
/// the other at its end, so the two words meet character by character.
const AT_START: &str = "Oat ";
const AT_END: &str = " today";
/// How much a channel may differ before a pixel counts as drawn again (antialiasing moves an edge
/// by a step or two without anything having happened).
const DRAWN: u8 = 8;

/// Open the checklist on both devices and wait until each window is drawn.
fn open_the_checklist(first: &Device, second: &Device) {
    for device in [first, second] {
        device.open(SHARED_CHECKLIST);
        device.wait_for_window(SHARED_CHECKLIST);
    }
}

/// The checklist as the app on `device` publishes it: its outline, the tree its window draws, with
/// every item's title as its value.
fn outline(device: &Device) -> Option<Value> {
    let answer = device.app_act(SHARED_CHECKLIST, "outline", &json!({}));
    answer.pointer("/update/output/value").cloned()
}

/// The one item's title the outline `value` holds, wherever the tree nests it.
fn title_in(value: &Value) -> Option<String> {
    match value {
        Value::Object(fields) => {
            if fields
                .get("control")
                .and_then(Value::as_str)
                .is_some_and(|control| control.starts_with("title:"))
            {
                if let Some(title) = fields.get("value").and_then(Value::as_str) {
                    return Some(title.to_owned());
                }
            }
            fields.values().find_map(title_in)
        }
        Value::Array(items) => items.iter().find_map(title_in),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => None,
    }
}

/// The one item's title as the app on `device` draws it.
fn title_on(device: &Device) -> Option<String> {
    title_in(&outline(device)?)
}

/// Wait until the app on `device` draws exactly `wanted` as its item's title.
#[track_caller]
fn wait_for_title(device: &Device, wanted: &str) -> String {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let read = title_on(device);
        if read.as_deref() == Some(wanted) {
            return wanted.to_owned();
        }
        assert!(
            Instant::now() < deadline,
            "{}'s checklist never read {wanted:?}; it read {read:?}\n{}",
            device.label,
            device.diagnostic()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Where the item's title stands on `device`'s screen, as the window describes it.
#[track_caller]
fn title_rect(device: &Device) -> [f64; 4] {
    let answer = device.ask(&json!({"op": "list_targets"}));
    let targets = answer["targets"]
        .as_array()
        .unwrap_or_else(|| panic!("{} lists its controls: {answer}", device.label));
    let title = targets
        .iter()
        .find(|target| {
            target["identity"]
                .as_str()
                .is_some_and(|identity| identity.starts_with("title:"))
        })
        .unwrap_or_else(|| panic!("{} shows no item's title: {targets:?}", device.label));
    let rect = title["rect"]
        .as_array()
        .filter(|rect| rect.len() == 4)
        .unwrap_or_else(|| panic!("{}'s title has a rect: {title}", device.label));
    [0_usize, 1, 2, 3].map(|at| {
        rect[at]
            .as_f64()
            .unwrap_or_else(|| panic!("{}'s title rect is numbers: {title}", device.label))
    })
}

/// Put the caret in the item's title on `device`, as a person pressing in it does.
#[track_caller]
fn press_in_the_title(device: &Device) {
    let [x, y, width, height] = title_rect(device);
    let answer = device.ask(&json!({
        "op": "click_at",
        "x": x + width / 2.0,
        "y": y + height / 2.0,
    }));
    assert!(
        answer.get("err").is_none(),
        "{} presses in its title: {answer}\n{}",
        device.label,
        device.diagnostic()
    );
}

/// Press the key `key` on `device`, as the person at its keyboard does.
#[track_caller]
fn press_key(device: &Device, key: &str) {
    let answer = device.ask(&json!({"op": "key", "key": key}));
    assert!(
        answer.get("err").is_none(),
        "{} presses {key}: {answer}\n{}",
        device.label,
        device.diagnostic()
    );
}

/// The checklist's versions as the app on `device` lists them, each with the member it is
/// attributed to: an entry that leaves `by` out is the version before's, as the answer says.
#[track_caller]
fn attributed_history(device: &Device) -> Vec<(String, Value)> {
    let answer = device.app_act(SHARED_CHECKLIST, "history", &json!({}));
    let versions = answer
        .pointer("/update/output/value/versions")
        .and_then(Value::as_array)
        .unwrap_or_else(|| {
            panic!(
                "{} lists its checklist's history: {answer}\n{}",
                device.label,
                device.diagnostic()
            )
        });
    let mut author = String::new();
    versions
        .iter()
        .map(|version| {
            if let Some(by) = version["by"].as_str() {
                author = by.to_owned();
            }
            (author.clone(), version.clone())
        })
        .collect()
}

/// What the version records: the action it ran, the field it edited, or the history move.
fn did(version: &Value) -> &str {
    version
        .pointer("/what/did")
        .and_then(Value::as_str)
        .unwrap_or_default()
}

/// Whether `version` is the item's title being edited.
fn is_title_edit(version: &Value) -> bool {
    did(version) == "edit"
        && version.pointer("/what/field").and_then(Value::as_str) == Some("title")
}

/// A picture of `device`'s screen, kept at `path`.
#[track_caller]
fn snapshot(device: &Device, path: &Path) {
    let answer = device.ask(&json!({"op": "snapshot", "path": path.display().to_string()}));
    assert!(
        answer.get("err").is_none(),
        "{} takes a picture: {answer}\n{}",
        device.label,
        device.diagnostic()
    );
}

/// The pixels of the picture at `path`.
#[track_caller]
fn pixels(path: &Path) -> image::RgbaImage {
    image::open(path)
        .unwrap_or_else(|error| panic!("{} reads: {error}", path.display()))
        .to_rgba8()
}

/// Wait until `device` draws the title's region twice the same, and answer that picture: what the
/// title looks like once nothing in it is moving.
#[track_caller]
fn rest_within(device: &Device, home: &Path, rect: [f64; 4]) -> image::RgbaImage {
    let deadline = tilo_test_env::hang_deadline();
    let picture = home.join("screen.png");
    let mut last: Option<image::RgbaImage> = None;
    loop {
        snapshot(device, &picture);
        let frame = pixels(&picture);
        if last
            .as_ref()
            .is_some_and(|last| changed_in(last, &frame, rect) == 0)
        {
            let _ = std::fs::remove_file(&picture);
            return frame;
        }
        last = Some(frame);
        assert!(
            Instant::now() < deadline,
            "{}'s title never came to rest\n{}",
            device.label,
            device.diagnostic()
        );
        std::thread::sleep(Duration::from_millis(60));
    }
}

/// Wait until the title on `device` is drawn differently from `before`, and answer the picture
/// that shows it.
#[track_caller]
fn wait_for_the_title_to_change(
    device: &Device,
    home: &Path,
    before: &image::RgbaImage,
    rect: [f64; 4],
    what: &str,
) -> image::RgbaImage {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let frame = rest_within(device, home, rect);
        if changed_in(before, &frame, rect) > 0 {
            return frame;
        }
        assert!(
            Instant::now() < deadline,
            "{}'s title never drew {what}\n{}",
            device.label,
            device.diagnostic()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// How many pixels of `rect` differ between the two pictures.
fn changed_in(a: &image::RgbaImage, b: &image::RgbaImage, rect: [f64; 4]) -> usize {
    let x0 = rect[0].max(0.0) as u32;
    let y0 = rect[1].max(0.0) as u32;
    let x1 = ((rect[0] + rect[2]).max(0.0) as u32).min(a.width());
    let y1 = ((rect[1] + rect[3]).max(0.0) as u32).min(a.height());
    let mut changed = 0;
    for y in y0..y1 {
        for x in x0..x1 {
            let p = a.get_pixel(x, y).0;
            let q = b.get_pixel(x, y).0;
            if p.iter().zip(q.iter()).any(|(p, q)| p.abs_diff(*q) > DRAWN) {
                changed += 1;
            }
        }
    }
    changed
}

#[test]
fn an_edit_on_one_members_checklist_reaches_the_other_with_its_caret_and_history() {
    let (site, ada, bob, first, second) =
        two_people_at_a_site_carrying(&[], |device| device.install_shared_checklist());
    show(&first, &site);
    show(&second, &site);
    // Both devices read the workspace they joined as their person: a refused read is why the
    // checklist its app draws never fills in, and nothing can be typed into either window.
    for device in [&first, &second] {
        device.wait_reading(&site.workspace);
    }
    open_the_checklist(&first, &second);

    // The checklist starts empty on both devices: nobody has put anything on it yet.
    assert!(
        title_on(&first).is_none(),
        "{} opens an empty checklist, not {:?}\n{}",
        first.label,
        title_on(&first),
        first.diagnostic()
    );

    // The first member adds an item, through the action its Add button invokes.
    let added = first.app_act(SHARED_CHECKLIST, "add", &json!({"title": ADDED}));
    assert!(
        added.get("err").is_none(),
        "{} adds {ADDED:?}: {added}\n{}",
        first.label,
        first.diagnostic()
    );
    wait_for_title(&first, ADDED);

    // The other's open checklist shows the same item: the record reached its device, and its app
    // merged the change into its own document.
    assert_eq!(wait_for_title(&second, ADDED), ADDED);

    // Typed into the item's title on the first device, through its own window.
    press_in_the_title(&first);
    press_key(&first, "End");
    first.type_text(TYPED);
    let titled = format!("{ADDED}{TYPED}");
    wait_for_title(&first, &titled);

    // The other's open checklist shows the same words: a field edit is collaborative text, not a
    // value that replaces the other's.
    assert_eq!(wait_for_title(&second, &titled), titled);

    // The first member's caret arrives on the second device: it is drawn in the title at rest,
    // moves when they move it, and moves again, while the words stay the ones already read.
    let home = crate::common::private_home();
    let title_on_second = title_rect(&second);
    let rest = rest_within(&second, home.path(), title_on_second);
    press_key(&first, "Home");
    let at_start = wait_for_the_title_to_change(
        &second,
        home.path(),
        &rest,
        title_on_second,
        "the first's caret",
    );
    press_key(&first, "End");
    let at_end = wait_for_the_title_to_change(
        &second,
        home.path(),
        &at_start,
        title_on_second,
        "the caret move along it",
    );
    assert_ne!(
        changed_in(&at_start, &at_end, title_on_second),
        0,
        "the caret moved with the first member, so it is theirs and it is here"
    );
    assert_eq!(
        title_on(&second).as_deref(),
        Some(titled.as_str()),
        "the caret moved and no word did\n{}",
        second.diagnostic()
    );

    // And the other way: the second member's caret arrives on the first device and moves with
    // them. Only the title's start is read there, where the second member's caret goes: the
    // first member's own caret stands at its end.
    let start_on_first = {
        let [x, y, width, height] = title_rect(&first);
        [x, y, width * 0.4, height]
    };
    let rest = rest_within(&first, home.path(), start_on_first);
    press_in_the_title(&second);
    press_key(&second, "Home");
    let at_start = wait_for_the_title_to_change(
        &first,
        home.path(),
        &rest,
        start_on_first,
        "the second's caret",
    );
    for _ in 0..3 {
        press_key(&second, "Right");
    }
    let along = wait_for_the_title_to_change(
        &first,
        home.path(),
        &at_start,
        start_on_first,
        "the second's caret move along it",
    );
    assert_ne!(
        changed_in(&at_start, &along, start_on_first),
        0,
        "the caret moved with the second member, so it is theirs and it is here"
    );
    assert_eq!(
        title_on(&first).as_deref(),
        Some(titled.as_str()),
        "the caret moved and no word did\n{}",
        first.diagnostic()
    );

    // Both members type in the one title at once, each at their own caret: the first at its end,
    // the second at its start, neither waiting for the other's words. The two runs of characters
    // meet in the middle instead of one overwriting the other.
    press_in_the_title(&second);
    press_key(&second, "Home");
    press_key(&first, "End");
    first.type_text(AT_END);
    second.type_text(AT_START);
    let merged = format!("{AT_START}{titled}{AT_END}");
    assert_eq!(wait_for_title(&second, &merged), merged);
    assert_eq!(wait_for_title(&first, &merged), merged);

    // One undo takes back the first member's own step, and only that: both devices read the
    // second member's word still there.
    let undone = first.app_act(SHARED_CHECKLIST, "undo", &json!({}));
    assert!(
        undone.get("err").is_none(),
        "{} takes back its own step: {undone}\n{}",
        first.label,
        first.diagnostic()
    );
    let ada_hex = hex::encode(ada.public);
    let taken = undone
        .pointer("/update/output/value/undone")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("{} names the step it took back: {undone}", first.label));
    assert_eq!(taken.len(), 1, "one step went back: {undone}");
    assert_eq!(
        taken[0]
            .pointer("/actor/by/member/person")
            .and_then(Value::as_str),
        Some(ada_hex.as_str()),
        "the step taken back is the first member's: {undone}"
    );
    assert!(
        is_title_edit(&taken[0]),
        "the step taken back is their title edit: {undone}"
    );
    let after = format!("{AT_START}{titled}");
    assert_eq!(wait_for_title(&first, &after), after);
    assert_eq!(wait_for_title(&second, &after), after);

    // The second device's history names each edit by the member who made it: the first member's
    // added item and title edits, the second member's concurrent edit, and the first member's undo
    // taking back only their own step.
    let bob_hex = hex::encode(bob.public);
    let history = attributed_history(&second);
    assert!(
        history.iter().any(|(by, version)| {
            by == &ada_hex
                && did(version) == "verb"
                && version
                    .pointer("/what/action/action")
                    .and_then(Value::as_str)
                    == Some("add")
        }),
        "the second device reads the first's added item as theirs: {history:#?}"
    );
    assert!(
        history
            .iter()
            .any(|(by, version)| by == &ada_hex && is_title_edit(version)),
        "the second device reads the first's title edits as theirs: {history:#?}"
    );
    assert!(
        history
            .iter()
            .any(|(by, version)| by == &bob_hex && is_title_edit(version)),
        "the second device reads its own title edit as its member's: {history:#?}"
    );
    for (by, version) in &history {
        if is_title_edit(version) {
            assert!(
                by == &ada_hex || by == &bob_hex,
                "every title edit names one of the two members, not {by:?}: {version}"
            );
        }
    }
    let undo_step = history
        .iter()
        .find(|(by, version)| {
            by == &ada_hex
                && did(version) == "history"
                && version.pointer("/what/op").and_then(Value::as_str) == Some("undo")
        })
        .unwrap_or_else(|| {
            panic!("the second device reads the first's undo as theirs: {history:#?}")
        });
    let taken_back: Vec<&str> = undo_step.1["takes_back"]
        .as_array()
        .unwrap_or_else(|| panic!("the undo names what it took back: {:#?}", undo_step.1))
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(
        !taken_back.is_empty(),
        "the undo names its step: {undo_step:#?}"
    );
    for short in taken_back {
        let (by, version) = history
            .iter()
            .find(|(_, version)| version["change"].as_str() == Some(short))
            .unwrap_or_else(|| panic!("the undo names a step history lists: {short}"));
        assert_eq!(
            by, &ada_hex,
            "the undo took back only the first member's step"
        );
        assert!(
            is_title_edit(version),
            "the step it took back is a title edit: {version}"
        );
    }
    assert!(
        !history.iter().any(
            |(by, version)| by == &bob_hex && version.get("undone") == Some(&Value::Bool(true))
        ),
        "the second member's edit stayed: {history:#?}"
    );
}
