//! One workspace, one note, two members' devices: the words typed into Scratchpad on the first
//! member's laptop arrive in the window open on the other member's, with the typist's caret; the
//! second member's undo takes back only the word they typed after them; and the note's history
//! still shows the first member's edit.
//!
//! Both devices are real sessions, joined to one workspace a site serves, each running the
//! installed Scratchpad: the note is the workspace's record, so this is the path a person takes
//! when they type on one device and look at the other.

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
use tilo_actions::remote::{RemoteActionMessage, ResourceReadRequest};

use crate::common::people::{Device, show, two_people_at_a_site_carrying};

/// The app both devices have installed and open.
const SCRATCHPAD: &str = tilo_app_id::apps::SCRATCHPAD;
/// The note's editor, as the window names it wherever its words are drawn.
const EDITOR: &str = "note-editor";
/// What the first member types: one line, so a caret at its start and its end stand apart.
const TYPED: &str = "Bread, milk and the tall blue candle";
/// What the second member types after those words arrive, and one undo takes back.
const ANSWERED: &str = "later";
/// How much a channel may differ before a pixel counts as drawn again (antialiasing moves an
/// edge by a step or two without anything having happened).
const DRAWN: u8 = 8;

/// Open Scratchpad on both devices and wait until each window is drawn.
fn open_the_note(first: &Device, second: &Device) {
    for device in [first, second] {
        device.open(SCRATCHPAD);
        device.wait_for_window(SCRATCHPAD);
    }
}

/// The note as `device`'s workspace service keeps it: its document, as the window draws it.
#[track_caller]
fn note_document(device: &Device, workspace: &str) -> Option<automerge::AutoCommit> {
    let result = match device.frame(RemoteActionMessage::ResourceRead {
        correlation: 1,
        on_behalf_of: None,
        read: ResourceReadRequest {
            workspace: workspace.to_owned(),
            app: SCRATCHPAD.to_owned(),
            // The app's one record of the workspace.
            record: String::new(),
            joined: false,
            after: None,
            wait_ms: Some(1_000),
            answer: tilo_actions::remote::ReadAnswer::Document,
        },
    }) {
        RemoteActionMessage::ResourceReading { result, .. } => result,
        other => panic!("{} answers a record read with {other:?}", device.label),
    };
    let readings: Value = serde_json::from_str(
        &result.unwrap_or_else(|error| panic!("{} reads the note: {error}", device.label)),
    )
    .expect("a reading is JSON");
    let document = readings["records"]
        .as_array()
        .and_then(|records| records.first())
        .and_then(|record| record["document"].as_str())?;
    let bytes = hex::decode(document).expect("a document is hex");
    Some(automerge::AutoCommit::load(&bytes).expect("the note is an automerge document"))
}

/// The note's body at `heads` (the note as it stood then), or now.
#[track_caller]
fn body_at(
    document: &automerge::AutoCommit,
    heads: Option<&[automerge::ChangeHash]>,
) -> Option<String> {
    use automerge::ReadDoc as _;
    let (value, body) = document
        .get(automerge::ROOT, "body")
        .expect("the note's document reads")?;
    assert!(
        matches!(value, automerge::Value::Object(automerge::ObjType::Text)),
        "the note's body is a text object, not {value:?}"
    );
    match heads {
        Some(heads) => document.text_at(&body, heads).ok(),
        None => document.text(&body).ok(),
    }
}

/// The note as the app on `device` reads it: its body, the words its window draws.
fn note_text(device: &Device, workspace: &str) -> Option<String> {
    let document = note_document(device, workspace)?;
    body_at(&document, None)
}

/// Wait until the app on `device` reads exactly `wanted`.
#[track_caller]
fn wait_for_text(device: &Device, workspace: &str, wanted: &str) -> String {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let read = note_text(device, workspace);
        if read.as_deref() == Some(wanted) {
            return wanted.to_owned();
        }
        assert!(
            Instant::now() < deadline,
            "{}'s Scratchpad never read {wanted:?}; it read {read:?}\n{}",
            device.label,
            device.diagnostic()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Wait until the app on `device` reads words holding `wanted`, and answer what it read.
#[track_caller]
fn wait_for_words(device: &Device, workspace: &str, wanted: &str) -> String {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let read = note_text(device, workspace).unwrap_or_default();
        if read.contains(wanted) {
            return read;
        }
        assert!(
            Instant::now() < deadline,
            "{}'s Scratchpad never read {wanted:?}; it read {read:?}\n{}",
            device.label,
            device.diagnostic()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Where the note's editor stands on `device`'s screen, as the window describes it.
#[track_caller]
fn editor_rect(device: &Device) -> [f64; 4] {
    let answer = device.ask(&json!({"op": "list_targets"}));
    let targets = answer["targets"]
        .as_array()
        .unwrap_or_else(|| panic!("{} lists its controls: {answer}", device.label));
    let editor = targets
        .iter()
        .find(|target| {
            target["identity"].as_str() == Some(EDITOR)
                || target["name"].as_str() == Some(EDITOR)
                // The note's field keeps its id where the window names it by that, and is the
                // window's one field taking more than a line where it names it by what it does.
                || target["role"].as_str() == Some("MultilineTextInput")
        })
        .unwrap_or_else(|| panic!("{} shows no {EDITOR}: {targets:?}", device.label));
    let rect = editor["rect"]
        .as_array()
        .filter(|rect| rect.len() == 4)
        .unwrap_or_else(|| panic!("{}'s editor has a rect: {editor}", device.label));
    [0_usize, 1, 2, 3].map(|at| {
        rect[at]
            .as_f64()
            .unwrap_or_else(|| panic!("{}'s editor rect is numbers: {editor}", device.label))
    })
}

/// Put the caret in the note's editor on `device`, as a person pressing in it does.
#[track_caller]
fn press_in_the_note(device: &Device) {
    let [x, y, width, height] = editor_rect(device);
    let answer = device.ask(&json!({
        "op": "click_at",
        "x": x + width / 2.0,
        "y": y + height / 2.0,
    }));
    assert!(
        answer.get("err").is_none(),
        "{} presses in its note: {answer}\n{}",
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

/// Take back the last change `device`'s open app made, through the undo its window bar and its
/// shortcut reach.
#[track_caller]
fn undo(device: &Device) {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        if let Some(target) = device.offered("tilo.host:edit", "undo") {
            let answer = device.act_wait(&target, &json!({}));
            assert!(
                answer.get("err").is_none(),
                "{} takes back its word: {answer}\n{}",
                device.label,
                device.diagnostic()
            );
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{} never offered undo after typing\n{}",
            device.label,
            device.diagnostic()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The note's text at the version `version` (a change's hash or a causal frontier), read by the
/// app on `device` from the history its document keeps.
#[track_caller]
fn text_at_version(device: &Device, workspace: &str, version: &str) -> Option<String> {
    let heads: Vec<automerge::ChangeHash> = version
        .split('+')
        .map(|hash| hash.parse().expect("a version names change hashes"))
        .collect();
    let document = note_document(device, workspace)?;
    body_at(&document, Some(&heads))
}

/// The heads of the note's record as `device`'s workspace service keeps it: the frontier a
/// version names, once another device's edit has arrived there.
#[track_caller]
fn record_heads(device: &Device, workspace: &str) -> Vec<String> {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let result = match device.frame(RemoteActionMessage::ResourceRead {
            correlation: 1,
            on_behalf_of: None,
            read: ResourceReadRequest {
                workspace: workspace.to_owned(),
                app: SCRATCHPAD.to_owned(),
                // The app's one record of the workspace.
                record: String::new(),
                joined: false,
                after: None,
                wait_ms: Some(1_000),
                answer: tilo_actions::remote::ReadAnswer::Document,
            },
        }) {
            RemoteActionMessage::ResourceReading { result, .. } => result,
            other => panic!("{} answers a record read with {other:?}", device.label),
        };
        let readings: Value = serde_json::from_str(
            &result.unwrap_or_else(|error| panic!("{} reads the note: {error}", device.label)),
        )
        .expect("a reading is JSON");
        let document = readings["records"]
            .as_array()
            .and_then(|records| records.first())
            .and_then(|record| record["document"].as_str());
        if let Some(document) = document {
            let bytes = hex::decode(document).expect("a document is hex");
            let mut note =
                automerge::AutoCommit::load(&bytes).expect("the note is an automerge document");
            return note.get_heads().iter().map(ToString::to_string).collect();
        }
        assert!(
            Instant::now() < deadline,
            "{} never kept a document of the note: {readings}\n{}",
            device.label,
            device.diagnostic()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
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

/// Wait until `device` draws the note's editor region twice the same, and answer that picture:
/// what the note looks like once nothing in it is moving.
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
            "{}'s note never came to rest\n{}",
            device.label,
            device.diagnostic()
        );
        std::thread::sleep(Duration::from_millis(60));
    }
}

/// Wait until the note's editor on `device` is drawn differently from `before`, and answer the
/// picture that shows it.
#[track_caller]
fn wait_for_the_note_to_change(
    device: &Device,
    home: &Path,
    before: &image::RgbaImage,
    what: &str,
) -> image::RgbaImage {
    let rect = editor_rect(device);
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let frame = rest_within(device, home, rect);
        if changed_in(before, &frame, rect) > 0 {
            return frame;
        }
        assert!(
            Instant::now() < deadline,
            "{}'s note never drew {what}\n{}",
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
fn an_edit_on_one_members_note_reaches_the_other_with_its_caret_and_history() {
    let (site, _ada, _bob, first, second) =
        two_people_at_a_site_carrying(&[], |device| device.install_scratchpad());
    show(&first, &site);
    show(&second, &site);
    open_the_note(&first, &second);

    // The note starts empty on both devices: nobody has written it yet.
    let opened = note_text(&first, &site.workspace);
    assert!(
        opened.as_deref().is_none_or(str::is_empty),
        "{} opens an empty note, not {opened:?}\n{}",
        first.label,
        first.diagnostic()
    );

    // Typed on the first device, through its own editor.
    press_in_the_note(&first);
    first.type_text(TYPED);
    wait_for_text(&first, &site.workspace, TYPED);

    // The other's open note shows the same words: the record reached its device, and its app
    // merged the changes into its own document.
    assert_eq!(wait_for_text(&second, &site.workspace, TYPED), TYPED);

    // The version the second device's history holds is the first's edit: nobody has written
    // there since.
    let version = record_heads(&second, &site.workspace).join("+");
    assert_eq!(
        text_at_version(&second, &site.workspace, &version).as_deref(),
        Some(TYPED),
        "the second Scratchpad reads the first's words as a version of the note\n{}",
        second.diagnostic()
    );

    // The first member's caret arrives on the second device: it is drawn in the note at rest,
    // moves when they move it, and moves again, while the words stay the ones already read.
    let home = crate::common::private_home();
    let note_on_second = editor_rect(&second);
    let rest = rest_within(&second, home.path(), note_on_second);
    press_key(&first, "Home");
    let at_start = wait_for_the_note_to_change(&second, home.path(), &rest, "the first's caret");
    press_key(&first, "End");
    let at_end =
        wait_for_the_note_to_change(&second, home.path(), &at_start, "the caret move along it");
    assert_ne!(
        changed_in(&at_start, &at_end, note_on_second),
        0,
        "the caret moved with the first member, so it is theirs and it is here"
    );
    assert_eq!(
        note_text(&second, &site.workspace).as_deref(),
        Some(TYPED),
        "the caret moved and no word did\n{}",
        second.diagnostic()
    );

    // The second member types their own word after the first's, at their own caret.
    press_in_the_note(&second);
    second.type_text(ANSWERED);
    let both = wait_for_words(&second, &site.workspace, ANSWERED);
    assert_eq!(
        wait_for_text(&first, &site.workspace, &both),
        both,
        "the first reads what the second typed"
    );

    // One undo takes back the second's word, and only that: both devices read the first's words
    // again.
    undo(&second);
    wait_for_text(&second, &site.workspace, TYPED);
    wait_for_text(&first, &site.workspace, TYPED);

    // And the first member's edit is still in the second device's history.
    assert_eq!(
        text_at_version(&second, &site.workspace, &version).as_deref(),
        Some(TYPED),
        "the second's history still holds the first's edit\n{}",
        second.diagnostic()
    );
}
