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

/// Every notice `device` was told, by its title.
fn told(device: &Device) -> Vec<String> {
    device.state()["state"]["notifications"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|notice| notice["title"].as_str().map(str::to_owned))
        .collect()
}

/// Assert `device` was never told a workspace did not take its layout: a device whose admission
/// has not arrived yet keeps its layout to itself, so the person hears nothing of a refusal they
/// can do nothing about.
#[track_caller]
fn never_told_of_a_layout(device: &Device) {
    let told = told(device);
    assert!(
        !told
            .iter()
            .any(|title| title.contains("did not take your window layout")),
        "{} was told its layout was refused: {told:?}\n{}\n{}",
        device.label,
        device.diagnostic(),
        layout_log(device),
    );
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
        "{} has here: {people:?}\nits shell says: {:?}\nits session warned:\n{}",
        device.label,
        words(device),
        device.warnings()
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

/// The path a picture of this run is kept at, where the run keeps pictures.
fn picture_at(name: &str) -> Option<std::path::PathBuf> {
    let directory = std::env::var_os("TL_SHELL_SHOTS")?;
    Some(std::path::Path::new(&directory).join(shot_name(name)))
}

/// Take a picture of `device`'s screen, where this run was asked to keep pictures, and the band
/// of the bar across its top beside it: what the bar says, close enough to look at.
#[track_caller]
fn snapshot(device: &Device, name: &str) {
    let Some(path) = picture_at(name) else {
        return;
    };
    std::fs::create_dir_all(path.parent().expect("a picture's directory"))
        .expect("the captures' directory");
    let answer = device.ask(&json!({"op": "snapshot", "path": path.to_string_lossy()}));
    assert!(
        answer.get("err").is_none(),
        "{} takes a picture at {}: {answer}\n{}",
        device.label,
        path.display(),
        device.diagnostic()
    );
    let picture = image::open(&path)
        .unwrap_or_else(|error| panic!("the picture at {}: {error}", path.display()))
        .to_rgba8();
    let band = image::imageops::crop_imm(&picture, 0, 0, picture.width(), BAR_HEIGHT).to_image();
    band.save(path.with_extension("band.png"))
        .unwrap_or_else(|error| panic!("the bar's band at {}: {error}", path.display()));
}

/// The rows of the screen the bar stands in.
const BAR_HEIGHT: u32 = 56;

/// How many pixels of the picture `name`, inside `rect`, are in a colour rather than in a grey:
/// a person's face wears their hue, and a glyph of the bar's ink has none.
#[track_caller]
fn coloured_in(name: &str, rect: [f64; 4], lower_right: bool) -> usize {
    let [x, y, width, height] = rect;
    let area = if lower_right {
        [x + width / 2.0, y + height / 2.0, width / 2.0, height / 2.0]
    } else {
        rect
    };
    let mut coloured = 0;
    coloured_pixels(name, area, 40, |_, _, _| coloured += 1);
    coloured
}

/// Every opaque pixel of picture `name` inside `rect` whose channels differ enough to carry a
/// hue (by `chroma`), passed to `take` as its red, green and blue. Nothing where this run keeps no
/// pictures or `name` was never taken.
fn coloured_pixels(name: &str, rect: [f64; 4], chroma: u8, mut take: impl FnMut(u8, u8, u8)) {
    let Some(path) = picture_at(name) else {
        return;
    };
    let Ok(picture) = image::open(&path) else {
        return;
    };
    let picture = picture.to_rgba8();
    let [x, y, width, height] = rect;
    let inside = |value: f64, limit: u32| {
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a place on the screen, in pixels"
        )]
        let pixel = value.round() as u32;
        pixel.min(limit.saturating_sub(1))
    };
    let (x0, x1) = (
        inside(x, picture.width()),
        inside(x + width, picture.width()),
    );
    let (y0, y1) = (
        inside(y, picture.height()),
        inside(y + height, picture.height()),
    );
    for at_y in y0..=y1 {
        for at_x in x0..=x1 {
            let [red, green, blue, alpha] = picture.get_pixel(at_x, at_y).0;
            if alpha < 128 {
                continue;
            }
            let high = red.max(green).max(blue);
            let low = red.min(green).min(blue);
            if high.saturating_sub(low) >= chroma {
                take(red, green, blue);
            }
        }
    }
}

/// Where a colour stands on the wheel, in degrees: red at 0, round through green and blue.
fn hue_of_colour(red: u8, green: u8, blue: u8) -> f64 {
    let (red, green, blue) = (f64::from(red), f64::from(green), f64::from(blue));
    let high = red.max(green).max(blue);
    let low = red.min(green).min(blue);
    let chroma = high - low;
    let sector = if high == red {
        ((green - blue) / chroma).rem_euclid(6.0)
    } else if high == green {
        (blue - red) / chroma + 2.0
    } else {
        (red - green) / chroma + 4.0
    };
    (sector * 60.0).rem_euclid(360.0)
}

/// The hue a face is drawn in inside `rect` of picture `name`: the mean direction of its coloured
/// pixels, or none where the rectangle holds no colour.
fn face_hue(name: &str, rect: [f64; 4]) -> Option<f64> {
    let (mut east, mut north, mut count) = (0.0, 0.0, 0usize);
    coloured_pixels(name, rect, 40, |red, green, blue| {
        let radians = hue_of_colour(red, green, blue).to_radians();
        east += radians.cos();
        north += radians.sin();
        count += 1;
    });
    (count > 0).then(|| north.atan2(east).to_degrees().rem_euclid(360.0))
}

/// The centre of a face's slot: the disc within its ring and ground gap, never the ring, so a
/// bare ring with a neutral disc inside it is not mistaken for a drawn face.
fn disc_in([x, y, width, height]: [f64; 4]) -> [f64; 4] {
    [x + width / 4.0, y + height / 4.0, width / 2.0, height / 2.0]
}

/// How far apart two hues stand on the wheel, in degrees: 0 to 180.
fn hue_apart(first: f64, second: f64) -> f64 {
    let apart = (first - second).abs() % 360.0;
    apart.min(360.0 - apart)
}

/// How many pixels inside `rect` of picture `name` are in a colour within `within` degrees of
/// `hue`: one person's face drawn again on another surface.
fn in_hue(name: &str, rect: [f64; 4], hue: f64, within: f64) -> usize {
    let mut found = 0;
    coloured_pixels(name, rect, 40, |red, green, blue| {
        if hue_apart(hue_of_colour(red, green, blue), hue) <= within {
            found += 1;
        }
    });
    found
}

/// Take the picture `name` once the frame it holds differs from `previous`'s: a word reaches
/// assistive technology a frame before the bar paints it, so a picture taken on the word alone can
/// catch the frame from before it. Where this run keeps no pictures, this is one ask and no more.
#[track_caller]
fn snapshot_changed(device: &Device, name: &str, previous: &str) {
    let (Some(path), Some(before)) = (picture_at(name), picture_at(previous)) else {
        snapshot(device, name);
        return;
    };
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(5))
        .expect("the bound fits in an Instant");
    loop {
        snapshot(device, name);
        let drew = std::fs::read(&path).ok();
        let was = std::fs::read(&before).ok();
        if drew.is_some() && drew != was {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{}'s bar never drew {name:?} apart from {previous:?}\n{}",
            device.label,
            explain(device),
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// What `device`'s session said of the workspace's shared layout: where a layout the workspace
/// did not take has to say why.
fn layout_log(device: &Device) -> String {
    let state = device.state();
    let workspaces = &state["state"]["peers"]["workspaces"];
    let refusals: Vec<Value> = workspaces
        .as_array()
        .into_iter()
        .flatten()
        .filter(|workspace| {
            ["write_refusals", "lasting_refusals"]
                .into_iter()
                .any(|field| {
                    workspace
                        .get(field)
                        .and_then(Value::as_array)
                        .is_some_and(|refusals| !refusals.is_empty())
                })
        })
        .cloned()
        .collect();
    format!("what the daemon refused:\n{refusals:#?}")
}

/// What `device`'s session said of the members it publishes to its shell, and of serving them:
/// where a face that never came has to say why.
fn members_log(device: &Device) -> String {
    device
        .log()
        .lines()
        .filter(|line| line.contains("members") || line.contains("people"))
        .rev()
        .take(40)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n")
}

/// Drop the pictures kept under `name` and its band: a frame a run only looked apart from.
fn discard(name: &str) {
    if let Some(path) = picture_at(name) {
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("band.png"));
    }
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
    crate::common::people::log_sessions_at("info,tilolang_tilo=debug,tilo_session::shell=debug");
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
    // What her bar draws with nobody come yet: the arrival's picture is taken apart from this one,
    // never from a frame still showing it.
    snapshot(&ada_laptop, "before-arrival");
    bob_laptop.begin(Some(&theirs));

    // Ada's bar says Bob is here on its own, in the bar, once her session knows he is here: the
    // arrival notice names the first word of his name, where the standing label beside the faces
    // names the whole of it. One cheap ask a frame watches both — his face, which appears as her
    // session knows him, and the notice — never the session's whole state dump, which on a loaded
    // machine can outlast the notice's four seconds between two looks.
    let mut knew_at: Option<Instant> = None;
    let arrival = waited(&ada_laptop, "Bob's arrival", JOINING, || {
        let shown = controls(&ada_laptop);
        let says = |name: &str| {
            shown
                .iter()
                .any(|control| control["name"].as_str() == Some(name))
        };
        if knew_at.is_none() && says("Bob Builder") {
            knew_at = Some(Instant::now());
        }
        if let Some(knew) = knew_at {
            assert!(
                knew.elapsed() < Duration::from_secs(10),
                "Ada's bar drew Bob's face but never said he arrived\n{}",
                explain(&ada_laptop),
            );
        }
        shown
            .into_iter()
            .find(|control| control["name"].as_str() == Some("Bob is here"))
    });
    let arrival_rect = rect(&arrival);
    let [_, y, _, height] = arrival_rect;
    assert!(
        y + height <= BAR,
        "the arrival notice stands in the bar: {arrival}"
    );
    snapshot_changed(&ada_laptop, "arrival", "before-arrival");
    discard("before-arrival");
    // The person at this device keeps their own face beside the news: no arrival turns their own
    // slot into a bare person icon while everyone else is drawn as themselves.
    let mine = rect(&waited(
        &ada_laptop,
        "the local person's face on the bar",
        STEP,
        || {
            controls(&ada_laptop).into_iter().find(|control| {
                control["name"]
                    .as_str()
                    .is_some_and(|name| name.ends_with(": open your card"))
            })
        },
    ));
    let disc = disc_in(mine);
    let own_deadline = Instant::now()
        .checked_add(STEP)
        .expect("the bound fits in an Instant");
    loop {
        snapshot(&ada_laptop, "own-face");
        let coloured = coloured_in("own-face", disc, false);
        if coloured >= 30 {
            break;
        }
        assert!(
            Instant::now() < own_deadline,
            "the local person is drawn as their face while an arrival is said: {coloured} coloured pixels in {disc:?}\n{}",
            members_log(&ada_laptop),
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    // No press took it away: the bar says it for its four seconds and then gives it up.
    gone(&ada_laptop, "Bob is here", Duration::from_secs(10));
    // Her bar with the notice gone, and the time back where it stands: the wave's picture is taken
    // apart from this one. His own session is up before her list is read, so her surfaces draw
    // him by the person the workspace and his presence name, not by the device key they heard
    // first.
    snapshot(&ada_laptop, "before-wave");
    bob_laptop.ready();
    bob_laptop.wait_holding(&site.workspace);
    bob_laptop.wait_showing(&site.workspace);
    site.wait_for_member(bob.public);
    // His face in her people list is a face: drawn in his colour, not a glyph of the bar's ink.
    // A person the shell's member reading has not listed yet stands on the bar as the person
    // icon, so the face every surface draws them is read where the list draws it, in its leading
    // column.
    ada_laptop.press("Bob Builder is here");
    let row = rect(&waited(&ada_laptop, "Bob's row", STEP, || {
        named(&ada_laptop, "Bob Builder")
            .into_iter()
            .find(|control| {
                let [_, y, width, _] = rect(control);
                // The row, not a face of the same name: a row spans the panel, a face is small.
                y > BAR && width > 200.0
            })
    }));
    let leading = [row[0], row[1], 44.0, row[3]];
    let deadline = Instant::now()
        .checked_add(STEP)
        .expect("the bound fits in an Instant");
    loop {
        snapshot(&ada_laptop, "roster");
        let coloured = coloured_in("roster", leading, false);
        if coloured >= 60 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the list draws Bob as his face, in his colour, not as a glyph: {coloured} coloured pixels in {leading:?}\n{}",
            members_log(&ada_laptop),
        );
        std::thread::sleep(Duration::from_millis(250));
    }
    // The person at this device is drawn as their face too, in their colour, where they stand
    // first in their own roster: the local slot is not a glyph beside the others' faces.
    let you = rect(&waited(&ada_laptop, "the local person's row", STEP, || {
        // The row says how available they are under their name, and a reader hears both.
        controls(&ada_laptop).into_iter().find(|control| {
            control["name"]
                .as_str()
                .is_some_and(|name| name == "You" || name.starts_with("You, "))
        })
    }));
    let you_leading = [you[0], you[1], 44.0, you[3]];
    let coloured = coloured_in("roster", you_leading, false);
    assert!(
        coloured >= 60,
        "the local person is drawn as their face, in their colour: {coloured} coloured pixels in {you_leading:?}\n{}",
        members_log(&ada_laptop),
    );
    ada_laptop.press("Close");
    // One person, one colour: Bob's face on the bar, on the people card and in the notice that
    // said he arrived is drawn in the hue of the face his row in the roster draws.
    // The widget comes back once the members list is down, so its own face is drawn where the
    // people card stands.
    appear(&ada_laptop, "Open People here");
    ada_laptop.press("Open People here");
    let card_button = rect(&appear(&ada_laptop, "Show who is here"));
    // The faces the card draws stand in its body above its way into the roster.
    let card_faces = [
        card_button[0] - 8.0,
        card_button[1] - 112.0,
        card_button[2] + 16.0,
        104.0,
    ];
    let bar_face = rect(&waited(&ada_laptop, "Bob's face on the bar", STEP, || {
        controls(&ada_laptop).into_iter().find(|control| {
            let [_, y, width, _] = rect(control);
            // The face on the bar, not a row of a panel: small, above the bar's line, and named
            // for the person as the bar names a face.
            y < BAR
                && width < 80.0
                && control["name"]
                    .as_str()
                    .is_some_and(|name| name == "Bob Builder" || name.starts_with("Bob Builder, "))
        })
    }));
    // Both the bar's face and the card's, in one frame: what each surface draws of him now.
    snapshot(&ada_laptop, "faces");
    let member_hue = face_hue("roster", leading).unwrap_or_else(|| {
        panic!(
            "the roster draws Bob's face in a colour: {} coloured pixels in {leading:?}",
            coloured_in("roster", leading, false),
        )
    });
    let [arrival_x, arrival_y, _, arrival_h] = arrival_rect;
    let pill = [
        arrival_x + 4.0,
        arrival_y + 4.0,
        (arrival_h - 8.0).max(16.0),
        (arrival_h - 8.0).max(16.0),
    ];
    for (surface, picture, area) in [
        ("the bar", "faces", bar_face),
        ("the people card", "faces", card_faces),
        ("the arrival notice", "arrival", pill),
    ] {
        let pixels = in_hue(picture, area, member_hue, 20.0);
        assert!(
            pixels >= 40,
            "{surface} draws Bob in the hue of his roster face ({member_hue:.0}°): {pixels} pixels of it in {area:?}\n{}",
            members_log(&ada_laptop),
        );
    }
    ada_laptop.press("Fold");
    // The newcomer's first looks write nothing of the shared layout until the workspace admits
    // him: nothing tells him his layout was refused.
    never_told_of_a_layout(&bob_laptop);
    if light() {
        // A device reads the appearance its person chose: his own bar is drawn in it too, so the
        // pictures of both bars are the theme this run is taking.
        choose_light(&bob_laptop);
    }

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
    // The local slot is one face in one colour: his own row in his own list wears the hue his
    // own face on his bar wears, and his face, never a glyph.
    let his_you = rect(&waited(&bob_laptop, "his own row", STEP, || {
        controls(&bob_laptop).into_iter().find(|control| {
            let [_, y, width, _] = rect(control);
            y > BAR
                && width > 200.0
                && control["name"]
                    .as_str()
                    .is_some_and(|name| name == "You" || name.starts_with("You, "))
        })
    }));
    let his_slot = rect(&waited(
        &bob_laptop,
        "his own face on his bar",
        STEP,
        || {
            controls(&bob_laptop).into_iter().find(|control| {
                control["name"]
                    .as_str()
                    .is_some_and(|name| name.ends_with(": open your card"))
            })
        },
    ));
    snapshot(&bob_laptop, "his-roster");
    let his_hue = face_hue("his-roster", [his_you[0], his_you[1], 44.0, his_you[3]])
        .expect("his own row draws his face in a colour");
    let his_slot_hue =
        face_hue("his-roster", his_slot).expect("his bar draws him in a colour, not a glyph");
    assert!(
        hue_apart(his_slot_hue, his_hue) <= 20.0,
        "the local person is one colour on the bar and in the list: {his_slot_hue:.0}° on his bar, {his_hue:.0}° in his list\n{}",
        members_log(&bob_laptop),
    );
    // His roster with the row's wave not yet pressed: the frame the sent wave's picture is told
    // apart from.
    snapshot(&bob_laptop, "before-waved");
    bob_laptop.press("Wave at Ada Lovelace");
    // The row says the wave went, to the eye and to assistive technology alike.
    says(&bob_laptop, "You waved at Ada Lovelace");
    snapshot_changed(&bob_laptop, "waved", "before-waved");
    discard("before-waved");

    // Ada's bar says who waved, beside her faces.
    says(&ada_laptop, "Bob Builder waved at you");
    snapshot_changed(&ada_laptop, "wave", "before-wave");
    discard("before-wave");

    // Bob follows Ada from her row: her view becomes the one his bar keeps in front.
    bob_laptop.press("Follow Ada Lovelace");
    says(&bob_laptop, "Following Ada Lovelace");
    // Ada's bar marks him as following her, and a reader hears it on the face's own control.
    says(&ada_laptop, "Bob Builder, follows you");
    snapshot(&ada_laptop, "follower");
    // His row in her people list says he follows her, as his face on the bar does: the mark the
    // shells draw for a follower, read where the list draws his row.
    ada_laptop.press("Bob Builder is here");
    says(&ada_laptop, "Follows you");
    ada_laptop.press("Close");

    // The wave at Ada goes by itself, once its six seconds are over: nothing about the wave was
    // pressed since it was heard.
    waited(
        &ada_laptop,
        "the wave at Ada to go",
        Duration::from_secs(20),
        || (!shows(&ada_laptop, "Bob Builder waved at you")).then_some(()),
    );

    // Bob stops following her from the same place his bar says whose view it is: the control
    // says whom it stops following.
    bob_laptop.press("Stop following Ada Lovelace");
    // Nothing on Ada's bar marks him as following her any more; his face stays, named plainly.
    waited(
        &ada_laptop,
        "Bob marked as following her to go",
        STEP,
        || (!shows(&ada_laptop, "Bob Builder, follows you")).then_some(()),
    );
    appear(&ada_laptop, "Bob Builder");
    snapshot(&ada_laptop, "follower-gone");
    // No look of the run told either device that a workspace did not take its layout.
    for device in [&ada_laptop, &bob_laptop] {
        never_told_of_a_layout(device);
    }
}

/// Someone nobody has named arrives where this person is: the bar says so, with their face, as
/// every surface says an unnamed person — "Someone is here" — and never leaves them out of the
/// news of who came.
#[test]
fn an_unnamed_person_arriving_is_said_on_the_bar() {
    let ada = Person::new("Ada Lovelace");
    // A person who has given no name yet, as someone joining before they name themselves.
    let someone = Person::new("");
    let site = Site::start(&ada, "Studio");
    let mut ada_laptop = ada.device("Ada's laptop");
    let hers = site.invite(ada.public, Role::Editor);
    ada_laptop.join(&hers);
    ada_laptop.wait_holding(&site.workspace);
    ada_laptop.wait_showing(&site.workspace);
    says(&ada_laptop, "People");
    if light() {
        choose_light(&ada_laptop);
    }
    let mut their_laptop = someone.device("A laptop");
    let invitation = site.invite(someone.public, Role::Editor);
    snapshot(&ada_laptop, "before-unnamed");
    their_laptop.begin(Some(&invitation));
    // The first word of the name every surface gives them: "Someone".
    appear(&ada_laptop, "Someone is here");
    assert!(
        shows(&ada_laptop, "Someone is here"),
        "the bar says who arrived"
    );
    snapshot_changed(&ada_laptop, "arrival-unnamed", "before-unnamed");
    discard("before-unnamed");
    // The bar's own slot draws her face once the pass that names who arrived has reached the
    // shell: the frame the arrival is first said can be one pass before the members are read.
    let mine = rect(&waited(
        &ada_laptop,
        "the local person's face on the bar",
        STEP,
        || {
            controls(&ada_laptop).into_iter().find(|control| {
                control["name"]
                    .as_str()
                    .is_some_and(|name| name.ends_with(": open your card"))
            })
        },
    ));
    let deadline = Instant::now()
        .checked_add(STEP)
        .expect("the bound fits in an Instant");
    let disc = disc_in(mine);
    loop {
        snapshot(&ada_laptop, "arrival-unnamed");
        let coloured = coloured_in("arrival-unnamed", disc, false);
        if coloured >= 30 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the local person is drawn as their face while an arrival is said: {coloured} coloured pixels in {disc:?}\n{}",
            members_log(&ada_laptop),
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}
