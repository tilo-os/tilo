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
        "{} was told its layout was refused: {told:?}\n{}",
        device.label,
        device.diagnostic()
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

/// How many pixels of the two pictures, inside `rect`, differ by more than a shade, and how many
/// of those stand in the lower half of the rect: where a follower's mark is drawn on a face.
#[track_caller]
fn differing_in(before: &str, after: &str, rect: [f64; 4]) -> (usize, usize) {
    let (Some(before), Some(after)) = (picture_at(before), picture_at(after)) else {
        return (0, 0);
    };
    let (Ok(was), Ok(is)) = (image::open(&before), image::open(&after)) else {
        return (0, 0);
    };
    let (was, is) = (was.to_rgba8(), is.to_rgba8());
    let [x, y, width, height] = rect;
    let place = |value: f64, limit: u32| {
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a place on the screen, in pixels"
        )]
        let pixel = value.round() as u32;
        pixel.min(limit.saturating_sub(1))
    };
    let (x0, x1) = (place(x, was.width()), place(x + width, was.width()));
    let (y0, y1) = (place(y, was.height()), place(y + height, was.height()));
    let middle = place(y + height / 2.0, was.height());
    let (mut changed, mut lower) = (0, 0);
    for at_y in y0..=y1 {
        for at_x in x0..=x1 {
            if at_x >= is.width() || at_y >= is.height() {
                continue;
            }
            let (a, b) = (was.get_pixel(at_x, at_y).0, is.get_pixel(at_x, at_y).0);
            let moved = (0..3).any(|channel| a[channel].abs_diff(b[channel]) > 8);
            if moved {
                changed += 1;
                if at_y > middle {
                    lower += 1;
                }
            }
        }
    }
    (changed, lower)
}

/// How many pixels of the picture `name`, inside `rect`, are in a colour rather than in a grey:
/// a person's face wears their hue, and a glyph of the bar's ink has none.
#[track_caller]
fn coloured_in(name: &str, rect: [f64; 4], lower_right: bool) -> usize {
    let Some(path) = picture_at(name) else {
        return 0;
    };
    let Ok(picture) = image::open(&path) else {
        return 0;
    };
    let picture = picture.to_rgba8();
    let [x, y, width, height] = rect;
    let (left, right) = if lower_right {
        (x + width / 2.0, x + width)
    } else {
        (x, x + width)
    };
    let (top, bottom) = if lower_right {
        (y + height / 2.0, y + height)
    } else {
        (y, y + height)
    };
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
        inside(left, picture.width()),
        inside(right, picture.width()),
    );
    let (y0, y1) = (
        inside(top, picture.height()),
        inside(bottom, picture.height()),
    );
    let mut coloured = 0;
    for at_y in y0..=y1 {
        for at_x in x0..=x1 {
            let pixel = picture.get_pixel(at_x, at_y);
            let [red, green, blue, alpha] = pixel.0;
            if alpha < 128 {
                continue;
            }
            let high = red.max(green).max(blue);
            let low = red.min(green).min(blue);
            if high.saturating_sub(low) >= 40 {
                coloured += 1;
            }
        }
    }
    coloured
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

/// The shell `device` runs is the installed one, as a person's device runs it. A session whose
/// store holds no shell draws the build's own copy, which keeps its records in this session's
/// memory and so reads none of the workspace's members: its bar names nobody by their face and
/// marks no follower. The installed shell keeps its record in the workspace the person is in and
/// reads who is in it through the device's daemon.
fn run_the_installed_shell(device: &Device) {
    crate::common::dev_apps::install_named(&device.store(), &["so.tilo.shell"]);
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
    let ada = Person::new("Ada Lovelace");
    let bob = Person::new("Bob Builder");
    let site = Site::start(&ada, "Studio");
    let mut ada_laptop = ada.device("Ada's laptop");
    run_the_installed_shell(&ada_laptop);
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
    run_the_installed_shell(&bob_laptop);
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
    let [_, y, _, height] = rect(&arrival);
    assert!(
        y + height <= BAR,
        "the arrival notice stands in the bar: {arrival}"
    );
    snapshot_changed(&ada_laptop, "arrival", "before-arrival");
    discard("before-arrival");
    // His face on her bar is a face: drawn in his colour, not a glyph of the bar's ink.
    let face = rect(&appear(&ada_laptop, "Bob Builder"));
    let coloured = coloured_in("arrival", face, false);
    assert!(
        coloured >= 60,
        "the bar draws Bob as his face, in his colour, not as a glyph: {coloured} coloured pixels in {face:?}"
    );
    // No press took it away: the bar says it for its four seconds and then gives it up.
    gone(&ada_laptop, "Bob is here", Duration::from_secs(10));
    // Her bar with the notice gone, and the time back where it stands: the wave's picture is taken
    // apart from this one.
    snapshot(&ada_laptop, "before-wave");
    bob_laptop.ready();
    bob_laptop.wait_holding(&site.workspace);
    bob_laptop.wait_showing(&site.workspace);
    site.wait_for_member(bob.public);
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
    // Ada's bar marks his face as following her, and a reader hears it on the face's own control.
    says(&ada_laptop, "Bob Builder, follows you");
    snapshot_changed(&ada_laptop, "follower", "wave");
    // The mark is drawn on his face, and on its lower edge, where the presence face's follower
    // badge stands: nothing else about the face changed between the two frames.
    let (changed, lower) = differing_in("wave", "follower", face);
    assert!(
        changed > 0 && lower.saturating_mul(2) >= changed,
        "following draws a mark on the face's lower edge: {changed} pixels changed, {lower} of them there"
    );

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
    snapshot_changed(&ada_laptop, "follower-gone", "follower");
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
    run_the_installed_shell(&ada_laptop);
    let hers = site.invite(ada.public, Role::Editor);
    ada_laptop.join(&hers);
    ada_laptop.wait_holding(&site.workspace);
    ada_laptop.wait_showing(&site.workspace);
    says(&ada_laptop, "People");
    if light() {
        choose_light(&ada_laptop);
    }
    let mut their_laptop = someone.device("A laptop");
    run_the_installed_shell(&their_laptop);
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
}
