//! Two people in one workspace, each at a device of their own, as the tilolang shell says them:
//! Bob arrives and Ada's bar says so, on its own; Bob waves and Ada's bar says who, and his own
//! roster row says the wave went; Bob follows Ada and her bar marks his face as following her,
//! until he stops and it stops saying so.
//!
//! Both devices are real sessions of the kind a person runs, showing the tilolang shell, joined to
//! one workspace a site serves. Every check is what a shell draws or what assistive technology
//! hears on it; nothing here writes the reading it checks, so what is proven is the whole road
//! from one device's presence, through the workspace, to the words on the other person's bar. The
//! reading and the drawing on their own are `people_bar`'s.
//!
//! A run takes its pictures in the workspace's default dark theme; `TILO_PEOPLE_LIGHT` chooses the
//! light theme before the second person joins, so the same bar is captured in both.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the devices live until their \
              sessions end"
)]

use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tilo_server_protocol::workspaces::Role;

use crate::common::people::{Device, Person, Site};

/// Where the bar stands: a control on it is above this line, in logical pixels.
const BAR: f64 = 80.0;
/// How long a step on a shell that is already up is given: a step that takes longer has not
/// happened.
const STEP: Duration = Duration::from_secs(60);
/// How long a person who is joining at another device is given to be here: the join is the
/// product's own and takes its patience.
const JOINING: Duration = Duration::from_secs(240);

/// Whether the shell on `device` says `words`, as a screen reader reads its controls.
fn shows(device: &Device, words: &str) -> bool {
    let answer = device.ask(&json!({"op": "says", "text": words}));
    answer["answer"]["says"]
        .as_bool()
        .unwrap_or_else(|| panic!("{} answers `says` with {answer}", device.label))
}

/// Every control the shell on `device` publishes now: its name, where it stands, and what a press
/// on it does.
fn controls(device: &Device) -> Vec<Value> {
    device.ask(&json!({"op": "list_targets"}))["targets"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// The controls the shell on `device` publishes named exactly `name`, in the order they stand.
fn named(device: &Device, name: &str) -> Vec<Value> {
    controls(device)
        .into_iter()
        .filter(|control| control["name"].as_str() == Some(name))
        .collect()
}

/// The names every control on `device`'s shell goes by, in the order they stand.
fn words(device: &Device) -> Vec<String> {
    controls(device)
        .into_iter()
        .filter_map(|control| control["name"].as_str().map(str::to_owned))
        .collect()
}

/// Where the control `control` stands on the shell: `[x, y, width, height]`.
fn rect(control: &Value) -> [f64; 4] {
    let at = control["rect"]
        .as_array()
        .unwrap_or_else(|| panic!("a control has a place: {control}"));
    let part = |place: usize| {
        at.get(place)
            .and_then(Value::as_f64)
            .unwrap_or_else(|| panic!("a control's place is four numbers: {control}"))
    };
    [part(0), part(1), part(2), part(3)]
}

/// Who the session of `device` says is here in each workspace it takes part in, as its peer state
/// reads it: the rows the shell's faces are drawn from.
fn peers(device: &Device) -> Vec<Value> {
    let state = device.state();
    state["state"]["peers"]["workspaces"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|workspace| {
            workspace["members"]
                .as_array()
                .into_iter()
                .flatten()
                .cloned()
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Whether the session of `device` has `name` here in one of its workspaces.
fn here(device: &Device, name: &str) -> bool {
    peers(device)
        .iter()
        .any(|row| row["label"].as_str() == Some(name))
}

/// What `device` knows when a step fails: who its session has here, and every word its shell says.
fn explain(device: &Device) -> String {
    let people: Vec<String> = peers(device)
        .iter()
        .filter_map(|row| {
            Some(format!(
                "{} ({})",
                row["label"].as_str()?,
                row["kind"].as_str().unwrap_or("person")
            ))
        })
        .collect();
    format!(
        "{} has here: {people:?}\nits shell says: {:?}",
        device.label,
        words(device)
    )
}

/// Wait until `probe` answers, failing with who `device` has here and what its shell says when
/// `within` runs out.
#[track_caller]
fn waited<T>(
    device: &Device,
    what: &str,
    within: Duration,
    mut probe: impl FnMut() -> Option<T>,
) -> T {
    let deadline = Instant::now()
        .checked_add(within)
        .expect("the bound fits in an Instant");
    loop {
        if let Some(answered) = probe() {
            return answered;
        }
        assert!(
            Instant::now() < deadline,
            "{} never showed {what} within {within:?}\n{}",
            device.label,
            explain(device),
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Wait until the shell on `device` draws a control named exactly `name`, and answer it.
#[track_caller]
fn appear(device: &Device, name: &str) -> Value {
    waited(device, &format!("{name:?}"), STEP, || {
        named(device, name).into_iter().next()
    })
}

/// Wait until the shell on `device` says `words` at all, as a reader would hear them.
#[track_caller]
fn says(device: &Device, words: &str) {
    waited(device, &format!("{words:?}"), STEP, || {
        shows(device, words).then_some(())
    });
}

/// Wait until no control named exactly `name` is left on `device`, failing with what the shell
/// does say when `within` runs out: a notice that stays past its time is the bug this names.
#[track_caller]
fn gone(device: &Device, name: &str, within: Duration) {
    let deadline = Instant::now()
        .checked_add(within)
        .expect("the bound fits in an Instant");
    loop {
        if named(device, name).is_empty() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{}'s shell still says {name:?} after {within:?}\n{}",
            device.label,
            explain(device),
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Whether this run takes its pictures in the light theme.
fn light() -> bool {
    std::env::var_os("TILO_PEOPLE_LIGHT").is_some()
}

/// The name a picture of this run is kept under.
fn shot_name(name: &str) -> String {
    let theme = if light() { "light" } else { "dark" };
    format!("{name}-{theme}.png")
}

/// Take a picture of `device`'s screen, where this run was asked to keep pictures.
#[track_caller]
fn snapshot(device: &Device, name: &str) {
    let Some(directory) = std::env::var_os("TL_SHELL_SHOTS") else {
        return;
    };
    std::fs::create_dir_all(std::path::Path::new(&directory)).expect("the captures' directory");
    let path = std::path::Path::new(&directory).join(shot_name(name));
    let answer = device.ask(&json!({"op": "snapshot", "path": path.to_string_lossy()}));
    assert!(
        answer.get("err").is_none(),
        "{} takes a picture at {}: {answer}\n{}",
        device.label,
        path.display(),
        device.diagnostic()
    );
}

/// Choose the light theme in the workspace in front, as Settings' Appearance page or Quick
/// Settings' Dark mode tile does: the field's own published setter, which every device reads.
#[track_caller]
fn choose_light(device: &Device) {
    let set = |field: &str, value: &str| {
        let action = format!("set-{}", field.replace('_', "-"));
        let target = device
            .offered("settings/appearance", &action)
            .unwrap_or_else(|| {
                panic!(
                    "{} publishes the appearance's {action}\n{}",
                    device.label,
                    device.warnings(),
                )
            });
        let answer = device.act_wait(&target, &json!({ "value": value }));
        assert!(
            answer.get("err").is_none(),
            "{} sets {field} to {value}: {answer}\n{}",
            device.label,
            device.diagnostic()
        );
    };
    set("light_theme", "paper-light");
    set("mode", "light");
}

/// One workspace, two people, each at a device of their own: what the two bars say of each other
/// as they arrive, wave, follow and stop.
#[test]
fn two_people_arrive_wave_and_follow_on_each_others_bars() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let ada = Person::new("Ada Lovelace");
    let bob = Person::new("Bob Builder");
    let site = Site::start(&ada, "Studio");
    let mut ada_laptop = ada.device("Ada's laptop");
    let hers = site.invite(ada.public, Role::Editor);
    ada_laptop.join(&hers);
    ada_laptop.wait_holding(&site.workspace);
    ada_laptop.wait_showing(&site.workspace);
    // Ada's shell is up with nobody else here: the bar offers who is here.
    says(&ada_laptop, "People");
    eprintln!("before Bob joins, {}", explain(&ada_laptop));
    if light() {
        // The workspace's theme before anyone arrives, so every picture this run takes is of the
        // same bar in the light theme.
        choose_light(&ada_laptop);
    }

    // Bob comes in at a device of his own, as the site's invitation admits him. His session starts
    // without waiting to listen, so Ada's bar is watched from before it could have said anything:
    // the arrival's four seconds are hers from the moment her session knows he is here.
    let mut bob_laptop = bob.device("Bob's laptop");
    let theirs = site.invite(bob.public, Role::Editor);
    bob_laptop.begin(Some(&theirs));

    // Ada's bar says Bob is here on its own, in the bar, once her session knows he is here: the
    // arrival notice names the first word of his name, where the standing label beside the faces
    // names the whole of it. Who her session has here is read a few times a second, not at every
    // frame: it is the whole state dump.
    let mut knew_at: Option<Instant> = None;
    let mut look_at = Instant::now();
    let arrival = waited(&ada_laptop, "Bob's arrival", JOINING, || {
        if knew_at.is_none() && Instant::now() >= look_at {
            look_at = Instant::now() + Duration::from_millis(250);
            if here(&ada_laptop, "Bob Builder") {
                knew_at = Some(Instant::now());
            }
        }
        if let Some(knew) = knew_at {
            assert!(
                knew.elapsed() < Duration::from_secs(10),
                "Ada's session had Bob here but her bar never said he arrived\n{}",
                explain(&ada_laptop),
            );
        }
        named(&ada_laptop, "Bob is here").into_iter().next()
    });
    let [_, y, _, height] = rect(&arrival);
    assert!(
        y + height <= BAR,
        "the arrival notice stands in the bar: {arrival}"
    );
    snapshot(&ada_laptop, "arrival");
    // No press took it away: the bar says it for its four seconds and then gives it up.
    gone(&ada_laptop, "Bob is here", Duration::from_secs(10));
    bob_laptop.ready();
    bob_laptop.wait_holding(&site.workspace);
    bob_laptop.wait_showing(&site.workspace);
    site.wait_for_member(bob.public);

    // Bob's own bar says Ada is here, as Ada's said of him, and the label beside the faces names
    // her whole name; a press on it opens who is here.
    says(&bob_laptop, "Ada Lovelace is here");
    bob_laptop.press("Ada Lovelace is here");
    // Her row holds the way to wave at her, drawn where a finger finds it.
    let wave = appear(&bob_laptop, "Wave at Ada Lovelace");
    let [_, _, width, height] = rect(&wave);
    assert!(
        width > 0.0 && height > 0.0,
        "the row's wave stands on the shell: {wave}"
    );
    bob_laptop.press("Wave at Ada Lovelace");
    // The row says the wave went, to the eye and to assistive technology alike.
    says(&bob_laptop, "You waved at Ada Lovelace");
    snapshot(&bob_laptop, "waved");

    // Ada's bar says who waved, beside her faces.
    says(&ada_laptop, "Bob Builder waved at you");
    snapshot(&ada_laptop, "wave");

    // Bob follows Ada from her row: her view becomes the one his bar keeps in front.
    bob_laptop.press("Follow Ada Lovelace");
    says(&bob_laptop, "Following Ada Lovelace");
    // Ada's bar marks his face as following her, and a reader hears it on the face's own control.
    says(&ada_laptop, "Bob Builder, follows you");
    snapshot(&ada_laptop, "follower");

    // The wave at Ada goes by itself, once its six seconds are over: nothing about the wave was
    // pressed since it was heard.
    waited(
        &ada_laptop,
        "the wave at Ada to go",
        Duration::from_secs(20),
        || (!shows(&ada_laptop, "Bob Builder waved at you")).then_some(()),
    );

    // Bob stops following her from the same place his bar says whose view it is.
    bob_laptop.press("Stop");
    // Nothing on Ada's bar marks him as following her any more; his face stays, named plainly.
    waited(
        &ada_laptop,
        "Bob marked as following her to go",
        STEP,
        || (!shows(&ada_laptop, "Bob Builder, follows you")).then_some(()),
    );
    appear(&ada_laptop, "Bob Builder");
    snapshot(&ada_laptop, "follower-gone");
}
