//! One keyboard and pointer for two of a person's devices standing side by side: the pointer
//! pushed past the laptop's edge toward the phone's screen goes on there while the laptop's own
//! waits out of sight, the keys typed on the laptop reach the phone, what was copied last pastes
//! on whichever device they are on, and Ctrl+Alt+Escape on the laptop brings both back. Both devices run
//! their real sessions, joined to one workspace as the same person, so the input travels the
//! person's own device fabric and the phone's compositor takes it as it takes remote input.

#![cfg(target_os = "linux")]

use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::common::people::{Device, two_devices_of_one_person};

/// What `device` says of the person's one keyboard and pointer.
fn shared(device: &Device) -> Value {
    device.state()["state"]["shared_input"].clone()
}

/// Whether `device`'s pointer shows on its screen.
fn pointer_shown(device: &Device) -> bool {
    shared(device)["pointer_shown"] == json!(true)
}

/// The text on `device`'s clipboard.
fn clipboard(device: &Device) -> Option<String> {
    device.state()["state"]["clipboard"]
        .as_str()
        .map(str::to_owned)
}

/// Copy `text` on `device`, as an app there does when the person copies: through the clipboard's
/// own copy action, as the device publishes it.
fn copy(device: &Device, text: &str) {
    const COPY: &str = "session:clipboard\u{1f}copy";
    let mut cursor = Value::Null;
    let provider = loop {
        let page = device.ask(&json!({"op": "actions_enumerate", "cursor": cursor}));
        let answer = &page["answer"];
        let found = answer["actions"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|row| row["id"] == json!(COPY));
        if let Some(row) = found {
            break row["provider"].as_str().expect("a provider").to_owned();
        }
        cursor = match answer["next_cursor"].as_str() {
            Some(next) if !next.is_empty() => Value::from(next),
            _ => panic!("{} publishes no clipboard copy: {page}", device.label),
        };
    };
    let receipt = device.act_wait(&provider, COPY, &json!({ "text": text }));
    assert!(receipt.get("err").is_none(), "{}: {receipt}", device.label);
}

/// Where `device`'s pointer is.
fn pointer(device: &Device) -> (f64, f64) {
    let at = &shared(device)["pointer"];
    (
        at[0].as_f64().expect("a pointer x"),
        at[1].as_f64().expect("a pointer y"),
    )
}

/// The device `device` drives now.
fn driving(device: &Device) -> Option<String> {
    shared(device)["driving"].as_str().map(str::to_owned)
}

/// Whether the overview is up on `device`.
fn overview_up(device: &Device) -> bool {
    device.state()["state"]["chrome"]["overlays"]["overview"] == json!(true)
}

/// Wait until `done` holds, telling what `devices` did when it never does.
fn waited(what: &str, devices: &[&Device], mut done: impl FnMut() -> bool) {
    let deadline = tilo_test_env::hang_deadline();
    while !done() {
        assert!(
            Instant::now() < deadline,
            "{what} never happened:\n{}",
            devices
                .iter()
                .map(|device| format!(
                    "{}: {}\n{}",
                    device.label,
                    shared(device),
                    device.diagnostic()
                ))
                .collect::<Vec<_>>()
                .join("\n\n")
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn ok(device: &Device, request: &Value) {
    let answer = device.ask(request);
    assert!(
        answer.get("err").is_none(),
        "{} {request}: {answer}",
        device.label
    );
}

fn move_to(device: &Device, (x, y): (f64, f64)) {
    ok(device, &json!({"op": "pointer_move", "x": x, "y": y}));
}

fn key(device: &Device, op: &str, key: &str) {
    ok(device, &json!({"op": op, "key": key}));
}

/// A screen of the arrangement as a device reads it: its device, whether it is its own, its place
/// in the arrangement and on its own device.
struct Screen {
    device: String,
    local: bool,
    rect: [f64; 4],
    local_rect: [f64; 4],
}

fn screens(device: &Device) -> Vec<Screen> {
    let rect = |value: &Value| -> [f64; 4] {
        let numbers: Vec<f64> = value
            .as_array()
            .expect("a rectangle")
            .iter()
            .map(|number| number.as_f64().expect("a number"))
            .collect();
        [numbers[0], numbers[1], numbers[2], numbers[3]]
    };
    shared(device)["displays"]
        .as_array()
        .map(|displays| {
            displays
                .iter()
                .filter(|display| display["online"] == json!(true))
                .map(|display| Screen {
                    device: display["device"].as_str().unwrap_or_default().to_owned(),
                    local: display["local"] == json!(true),
                    rect: rect(&display["rect"]),
                    local_rect: rect(&display["local_rect"]),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Save what `device` shows as `name` , when the run asked for pictures.
fn picture(device: &Device, name: &str) {
    let Some(dir) = std::env::var_os("TILO_SHARED_INPUT_SHOTS") else {
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    std::fs::create_dir_all(&dir).expect("a directory for the pictures");
    let path = dir.join(format!("{name}.png"));
    ok(
        device,
        &json!({"op": "snapshot", "path": path.display().to_string()}),
    );
}

#[test]
fn the_pointer_and_the_keyboard_cross_to_the_device_beside_this_one_and_come_back() {
    let (_site, _ada, laptop, phone) = two_devices_of_one_person();
    // Each device places the other's screen beside its own in the arrangement.
    waited(
        "the laptop and the phone seeing each other's screen",
        &[&laptop, &phone],
        || {
            [&laptop, &phone]
                .iter()
                .all(|device| screens(device).iter().any(|screen| !screen.local))
        },
    );
    let arranged = screens(&laptop);
    let here = arranged
        .iter()
        .find(|screen| screen.local)
        .expect("the laptop's screen");
    let there = arranged
        .iter()
        .find(|screen| !screen.local)
        .expect("the phone's screen");
    let phone_id = there.device.clone();
    // Which edge of the laptop's screen leads to the phone's, and the way across it.
    let [x, y, width, height] = here.rect;
    let toward: (f64, f64) = if there.rect[0] >= x + width {
        (1.0, 0.0)
    } else if there.rect[0] + there.rect[2] <= x {
        (-1.0, 0.0)
    } else if there.rect[1] >= y + height {
        (0.0, 1.0)
    } else {
        (0.0, -1.0)
    };
    let [lx, ly, lw, lh] = here.local_rect;
    let middle = (lx + lw / 2.0, ly + lh / 2.0);
    // The laptop's own point just inside that edge, and `by` pixels past it.
    let edge = |by: f64| {
        let x = if toward.0 > 0.0 {
            lx + lw - 1.0 + by
        } else if toward.0 < 0.0 {
            lx - by
        } else {
            middle.0
        };
        let y = if toward.1 > 0.0 {
            ly + lh - 1.0 + by
        } else if toward.1 < 0.0 {
            ly - by
        } else {
            middle.1
        };
        (x, y)
    };
    // Something copied on the laptop before the pointer leaves it.
    let from_laptop = "copied on the laptop";
    copy(&laptop, from_laptop);
    waited("the laptop's copy on its clipboard", &[&laptop], || {
        clipboard(&laptop).as_deref() == Some(from_laptop)
    });
    assert!(pointer_shown(&laptop), "the laptop shows its pointer");
    picture(&laptop, "1-laptop-before");
    picture(&phone, "1-phone-before");

    // The first push asks the phone for its keyboard and pointer; the phone grants them.
    move_to(&laptop, edge(0.0));
    move_to(&laptop, edge(10.0));
    waited(
        "the phone granting the laptop its input",
        &[&laptop, &phone],
        || {
            shared(&laptop)["leases"]
                .as_array()
                .is_some_and(|leases| leases.contains(&json!(phone_id)))
        },
    );

    // Resting against the edge, and pushing a little past it, keeps the pointer here.
    move_to(&laptop, edge(0.0));
    move_to(&laptop, edge(10.0));
    move_to(&laptop, edge(10.0));
    assert_eq!(
        driving(&laptop),
        None,
        "two short pushes stay on the laptop"
    );
    move_to(&laptop, edge(10.0));
    assert_eq!(
        driving(&laptop).as_deref(),
        Some(phone_id.as_str()),
        "the third push went through to the phone"
    );
    let parked = pointer(&laptop);
    waited(
        "the pointer arriving on the phone's screen",
        &[&laptop, &phone],
        || shared(&phone)["injected"].as_u64().unwrap_or(0) > 0,
    );
    let arrived = pointer(&phone);
    // The laptop's pointer waits out of sight at the edge, and what it copied pastes on the phone.
    waited("the laptop's pointer hiding", &[&laptop, &phone], || {
        !pointer_shown(&laptop)
    });
    assert!(pointer_shown(&phone), "the phone shows the pointer");
    waited(
        "the laptop's copy on the phone's clipboard",
        &[&laptop, &phone],
        || clipboard(&phone).as_deref() == Some(from_laptop),
    );

    // The pointer goes on across the phone's screen; the laptop's stays where it left.
    let along = 120.0;
    move_to(
        &laptop,
        (parked.0 + toward.0 * along, parked.1 + toward.1 * along),
    );
    waited(
        "the pointer moving on the phone",
        &[&laptop, &phone],
        || {
            let now = pointer(&phone);
            (now.0 - arrived.0).abs() + (now.1 - arrived.1).abs() >= along * 0.9
        },
    );
    assert_eq!(
        pointer(&laptop),
        parked,
        "the laptop's pointer waits at the edge"
    );

    // How long a step of the laptop's pointer takes to show on the phone.
    let mut took = Vec::new();
    for step in 1..=20 {
        let before = pointer(&phone);
        let offset = f64::from(step % 2) * 8.0 + 4.0;
        let started = Instant::now();
        move_to(
            &laptop,
            (parked.0 + toward.0 * offset, parked.1 + toward.1 * offset),
        );
        waited("one step showing on the phone", &[&laptop, &phone], || {
            pointer(&phone) != before
        });
        took.push(started.elapsed());
    }
    took.sort();
    let in_session = shared(&phone)["latency_ms"].clone();
    eprintln!(
        "shared input: a pointer step shows on the other device within {:?} (median) / {:?} \
         (worst of {}), measured through both control sockets; sent to injected in the \
         sessions: {in_session}",
        took[took.len() / 2],
        took[took.len() - 1],
        took.len()
    );

    // The keyboard follows the pointer: Super+Tab pressed on the laptop opens the phone's overview.
    key(&laptop, "key_down", "Super");
    key(&laptop, "key", "Tab");
    key(&laptop, "key_up", "Super");
    waited("the phone's overview opening", &[&laptop, &phone], || {
        overview_up(&phone)
    });
    assert!(
        !overview_up(&laptop),
        "the laptop's desk never saw the keys"
    );
    picture(&laptop, "2-laptop-driving-phone");
    picture(&phone, "2-phone-driven-by-laptop");

    // Something copied in an app on the phone, while the laptop's keys drive it.
    let from_phone = "copied on the phone";
    copy(&phone, from_phone);
    waited("the phone's copy on its clipboard", &[&phone], || {
        clipboard(&phone).as_deref() == Some(from_phone)
    });

    // Ctrl+Alt+Escape on the laptop brings its keyboard and pointer back.
    key(&laptop, "key_down", "Control");
    key(&laptop, "key_down", "Alt");
    key(&laptop, "key", "Escape");
    key(&laptop, "key_up", "Alt");
    key(&laptop, "key_up", "Control");
    assert_eq!(driving(&laptop), None, "the chord gave the input back");
    // The pointer shows again at the edge it left by, and what was copied on the phone pastes here.
    waited(
        "the laptop's pointer showing again",
        &[&laptop, &phone],
        || pointer_shown(&laptop),
    );
    assert_eq!(
        pointer(&laptop),
        parked,
        "the pointer comes back where it left"
    );
    waited(
        "the phone's copy on the laptop's clipboard",
        &[&laptop, &phone],
        || clipboard(&laptop).as_deref() == Some(from_phone),
    );
    move_to(&laptop, middle);
    assert_eq!(
        pointer(&laptop),
        middle,
        "the laptop's pointer moves here again"
    );
    let phone_now = pointer(&phone);
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(pointer(&phone), phone_now, "and no longer on the phone");
    picture(&laptop, "3-laptop-back");
    picture(&phone, "3-phone-after");
}
