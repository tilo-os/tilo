//! Desk styles as views of the arrangement (`docs/designs/app-language/shell.md` §6): every
//! installed package's styles are published on the desk reading, found by the type they draw,
//! and the desk draws the person's chosen one through `show`. A style is a view whose first
//! parameter is the session's arrangement — the type whose origin is `session.arrangement` —
//! never a keyword and never a list in code, so a package a person or their agent wrote offers
//! its styles beside the four the shell ships.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::arithmetic_side_effects,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and a session lives for its whole test"
)]

use std::time::Duration;

use tilo_compositor::{KeyRepeat, OutputConfig};
use tilo_session::{Options, Session};
use tilo_session_core::shell::DeskReading;
use tilo_surface_host::shell_part::DESK_RESOURCE;

const WIDTH: i32 = 1280;
const HEIGHT: i32 = 800;
const PERSON: &str = "Pat";

fn options() -> Options {
    Options {
        presentation: ui::headless::fixture_runner_inputs,
        person: PERSON.to_owned(),
        shell: true,
        ..Options::default()
    }
}

fn session() -> Session {
    let mut session =
        Session::headless(options(), &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)])
            .expect("session");
    session.compositor().set_key_repeat(KeyRepeat {
        delay: Duration::from_secs(1),
        rate: 0,
    });
    session
}

/// The desk as the session publishes it on its door: what an agent reads.
fn desk(session: &Session) -> DeskReading {
    serde_json::from_value(
        session
            .read(DESK_RESOURCE)
            .expect("the session publishes its desk"),
    )
    .expect("the desk reads as the shell's desk")
}

/// Run frames until `done` holds.
#[track_caller]
fn until(session: &mut Session, done: impl FnMut(&mut Session) -> bool) {
    crate::common::until(session, "the shell's answer", done);
}

/// Choose the style the switch lists as `label` (its doc line, or its view's name when it names
/// none) the way a person does: open the bar's style menu and press the style there.
#[track_caller]
fn choose(session: &mut Session, label: &str) {
    crate::common::show_preset(session, label);
}

/// A scratch directory a test writes a style package into, removed when the test ends.
struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        Self(std::env::temp_dir().join(format!(
            "{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("the clock")
                .as_nanos()
        )))
    }

    /// The package `source` written as `app.tl` under `name`, built and installed into `store`:
    /// the package id it installed as.
    fn install(&self, name: &str, source: &str, store: &std::path::Path) -> String {
        let dir = self.0.join(name);
        std::fs::create_dir_all(&dir).expect("a directory for the style package");
        std::fs::write(dir.join("app.ids"), "").expect("an empty ids lock");
        std::fs::write(dir.join("app.tl"), source).expect("the style package");
        crate::common::dev_apps::install_package(&dir, store)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

/// The style `style` (`app:view`) as the session publishes it: whether it loads and why not.
fn style_state(session: &Session, style: &str) -> Option<(bool, String)> {
    desk(session)
        .styles
        .into_iter()
        .find(|found| found.style == style)
        .map(|found| (found.loads, found.reason))
}

/// Step 2 of shell.md §6, "Styles are views": the four styles `so.tilo.shell` ships are views of
/// the arrangement, published on the desk's reading and listed by their doc lines, and the desk
/// draws the one the person chooses through `show`. Switching keeps the arrangement.
#[test]
fn the_four_shipped_styles_are_listed_and_drawn() {
    let _settled = ui::motion::testing::settled();
    crate::common::app_store();
    let mut session = session();
    // Frames until the shell has drawn its bar: a part reports its placement once it has, and
    // the bar's own style switch names the style the person shows.
    until(&mut session, |session| {
        session
            .accessible_labels()
            .iter()
            .any(|label| label == "Showing Strips")
    });
    // The styles the session read from its store: the shell's own four, each a view of the
    // arrangement, listed by its doc line.
    let styles = desk(&session).styles;
    assert!(!styles.is_empty(), "the desk offers styles: {styles:?}");
    for name in ["strips", "frames", "spaces", "desktop"] {
        let found = styles
            .iter()
            .find(|style| style.style == format!("so.tilo.shell:{name}"))
            .unwrap_or_else(|| panic!("the desk offers the {name} style: {styles:?}"));
        assert!(found.loads, "the shipped {name} style loads");
        assert!(
            !found.line.is_empty(),
            "the {name} style is named: {found:?}"
        );
    }
    // The switch lists them, and choosing one draws that style's own view of the arrangement.
    assert!(
        !session.shell_parts().is_empty(),
        "the shell stands on its display: {:?}",
        session.shell_parts()
    );
    choose(&mut session, "Frames");
    until(&mut session, |session| {
        session
            .accessible_labels()
            .iter()
            .any(|label| label.starts_with("Showing "))
    });
}

/// Step 2's part the shell alone cannot show: a desk style **another** installed package declares
/// is listed by the switch and, chosen, draws the desk.
///
/// The person's own package writes a view taking `shell.Arrangement` — the arrangement's type,
/// whose origin is `session.arrangement` — and nothing else. The session finds it by that type,
/// never by a list in code, and the desk draws the package's own view once it is chosen.
///
/// This fails without the change: before it `Viewing.showing` was a closed `Preset`, so no
/// package could offer a style; and a first cut that read the arrangement's name only from the
/// package's own `meta.types` never listed a second package's style at all.
#[test]
fn a_style_from_another_installed_package_is_listed_and_draws_the_desk() {
    let _settled = ui::motion::testing::settled();
    let store = crate::common::app_store();
    // The person's own style package, written into a scratch directory and installed into the
    // store as any package is: a view of the arrangement and nothing else, its one line naming
    // it in the switch.
    let scratch = std::env::temp_dir().join(format!(
        "desk-style-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("the clock")
            .as_nanos()
    ));
    let source = scratch.join("styles");
    std::fs::create_dir_all(&source).expect("a directory for the style package");
    std::fs::write(source.join("app.ids"), "").expect("an empty ids lock");
    std::fs::write(
        source.join("app.tl"),
        r#"app atest.timeline "A timeline"

use so.tilo.shell as shell

/// Timeline of the desk
view timeline(a: shell.Arrangement) {
    ui.Text("Timeline of {a.places.count()} places")
}
"#,
    )
    .expect("the style package");
    let installed = crate::common::dev_apps::install_package(&source, store);
    assert_eq!(installed, "atest.timeline", "the style package installs");

    let mut session = session();
    until(&mut session, |session| !session.shell_parts().is_empty());
    // The store moved, so the session reads it again: the styles it lists now hold the person's
    // own beside the shell's four.
    until(&mut session, |session| {
        desk(session)
            .styles
            .iter()
            .any(|style| style.style == "atest.timeline:timeline")
    });
    let styles = desk(&session).styles;
    let own = styles
        .iter()
        .find(|style| style.style == "atest.timeline:timeline")
        .expect("the person's style is listed");
    assert!(own.loads, "the person's style loads: {own:?}");
    assert_eq!(
        own.line, "Timeline of the desk",
        "listed by its author's doc line"
    );
    // Choosing it is one action on the person's viewing, as the switch runs, and the desk draws
    // the package's own view of the arrangement: its words stand on the screen. The bar's style
    // menu lists every style by the one name it has everywhere (`style_label`), and the person's
    // own stands beside the shell's four.
    //
    // This style draws only its own words and no bar (`shell.md` §6 lets a style draw what it
    // wills), so once it is chosen the bar's switch is gone: the desk's answer is its own drawn
    // text, not a menu title. Open the bar's style menu, press the person's style, and read the
    // desk's answer.
    let mut since = usize::MAX;
    crate::common::wait_for(&mut session, "the timeline to draw the desk", |session| {
        if session
            .accessible_labels()
            .iter()
            .any(|label| label.starts_with("Timeline of "))
        {
            return Some(());
        }
        if since < 4 {
            since = since.saturating_add(1);
            return None;
        }
        if crate::common::on_screen(session, "Timeline of the desk").is_some() {
            crate::common::press(session, "Timeline of the desk");
        } else if let Some(menu) = crate::common::preset_menu(session) {
            crate::common::press(session, &menu);
        }
        since = 0;
        None
    });
    std::fs::remove_dir_all(&scratch).ok();
}

/// A style whose package does not load — step 3's failure at the session (`shell.md` §6,
/// "Nothing can lock a person out").
///
/// A module the compiler rejects never reaches the store, so the store's guard is not this
/// path. A package that installs and whose payload the session cannot read is the one that
/// reaches a person's desk: the store's row and the manifest stand, the switch lists the style
/// from the metadata it reads, and the load fails — the desk then shows the built-in strips,
/// says why, and keeps the choice. The runtime's own test names the other half (a package the
/// runtime refuses by name:
/// `tilolang-runtime`'s `a_style_whose_package_does_not_load_draws_a_failed_state_and_is_reported`).
#[test]
fn a_style_whose_package_does_not_load_shows_the_strips_and_a_notice_and_keeps_the_choice() {
    let _settled = ui::motion::testing::settled();
    let store = crate::common::app_store();
    let scratch = Scratch::new("desk-style-broken");
    let installed = scratch.install("broken", WORKING_STYLE, store);
    assert_eq!(installed, "atest.broken", "the style package installs");
    // The payload's module is taken away: the store still names the package, the manifest and
    // the row stand, and reading its module fails — what a store holds after a damaged install,
    // and the "does not compile against the installed shell and session" path at the session.
    omit_module(store, "atest.broken");
    let mut session = session();
    until(&mut session, |session| !session.shell_parts().is_empty());
    until(&mut session, |session| {
        style_state(session, "atest.broken:broken").is_some()
    });
    choose(&mut session, "A broken style of the desk");
    let (_, reason) = crate::common::wait_for(
        &mut session,
        "the broken style to be marked not loading",
        |session| style_state(session, "atest.broken:broken").filter(|(loads, _)| !*loads),
    );
    assert!(!reason.is_empty(), "the reason is published: {reason:?}");

    // The person is told: a notice names the style and why.
    until(&mut session, |session| {
        session
            .notices()
            .log()
            .active()
            .iter()
            .any(|notice| notice.title == "Your desk style could not be shown")
    });
    // The desk shows the built-in strips, not a blank screen: the strips' own dock and bar stand
    // (the broken style draws nothing of the desk), and the bar keeps naming the person's choice
    // because the choice is theirs, never rewritten.
    until(&mut session, |session| {
        let labels = session.accessible_labels();
        labels.iter().any(|label| label == "Dock")
            && labels
                .iter()
                .any(|label| label == "Showing A broken style of the desk")
    });
    // The person's choice is kept as it was: the bar still names the broken style as what the
    // person picked, which the switch reads off `Viewing.style` — never rewritten to Strips.
    let kept = session
        .accessible_labels()
        .into_iter()
        .any(|label| label == "Showing A broken style of the desk");
    assert!(
        kept,
        "the person's choice is kept, never rewritten: {:?}",
        session.accessible_labels()
    );
}

/// The style the broken package held before its module was taken away: a plain style of the
/// arrangement that compiles and draws against the installed shell.
const WORKING_STYLE: &str = r#"app atest.broken "A broken style"

use so.tilo.shell as shell

/// A broken style of the desk
view broken(a: shell.Arrangement) {
    ui.Text("Nothing of the kind: {a.places.count()}")
}
"#;

/// Take the installed package `app`'s module's code away, keeping its metadata: the module a
/// store holds after the shell it was compiled against changed under it, so its description
/// still reads and its code exports nothing the runtime wants.
fn omit_module(store: &std::path::Path, app: &str) {
    let catalog = package_store::Catalog::at(store, None);
    let installed = app_catalogue::apps::read_one(&catalog, app)
        .expect("the store reads")
        .expect("the package is installed");
    let path = installed.payload_root.join("app.wasm");
    let wasm = std::fs::read(&path).expect("the installed module reads");
    let stripped = custom_sections_only(&wasm);
    assert!(
        tilolang_tilo::runtime::Meta::from_wasm(&stripped).is_some(),
        "the stripped module still carries its metadata"
    );
    // The store keeps an installed payload read-only, as a view of what it holds: the damage is
    // written under the file's own mode lifted for the write, and the mode is put back, so the
    // store holds what a damaged install leaves whoever runs the test.
    let kept = std::fs::metadata(&path)
        .expect("the installed module's mode reads")
        .permissions();
    let mut writable = kept.clone();
    std::os::unix::fs::PermissionsExt::set_mode(&mut writable, 0o644);
    std::fs::set_permissions(&path, writable).expect("the installed module opens for the write");
    std::fs::write(&path, &stripped).expect("the stripped module is written");
    std::fs::set_permissions(&path, kept).expect("the installed module's mode is put back");
}

/// `wasm`'s header and its custom sections alone: every code, type, export and function
/// section is dropped, so it is a valid module that exports nothing. A tilolang module's
/// metadata rides in a custom section, so the description still reads and the code the runtime
/// checks for is gone — the module "does not compile against" what the runtime holds.
fn custom_sections_only(wasm: &[u8]) -> Vec<u8> {
    fn leb(value: u32, out: &mut Vec<u8>) {
        let mut value = value;
        loop {
            let byte = u8::try_from(value & 0x7f).unwrap_or(0x7f);
            value >>= 7;
            if value == 0 {
                out.push(byte);
                return;
            }
            out.push(byte | 0x80);
        }
    }
    let mut out = wasm.get(..8).unwrap_or_default().to_vec();
    let mut at = 8;
    while at < wasm.len() {
        let id = wasm[at];
        at += 1;
        let (mut size, mut shift) = (0u32, 0u32);
        loop {
            let Some(&byte) = wasm.get(at) else {
                return out;
            };
            at += 1;
            size |= u32::from(byte & 0x7f) << shift;
            shift += 7;
            if byte & 0x80 == 0 {
                break;
            }
        }
        let end = at.saturating_add(size as usize);
        let Some(body) = wasm.get(at..end) else {
            return out;
        };
        if id == 0 {
            out.push(0);
            leb(size, &mut out);
            out.extend_from_slice(body);
        }
        at = end;
    }
    out
}

/// A style that loads and then traps when it runs: a division by a count of nothing, which the
/// runtime refuses in the middle of the first frame.
const TRAPPING: &str = r#"app atest.trapping "A trappng style"

use so.tilo.shell as shell

/// A trapping style of the desk
view trapping(a: shell.Arrangement) {
    let places = a.places.count()
    ui.Text("at {places / 0} places")
}
"#;

#[test]
fn a_style_whose_view_traps_shows_the_strips_and_a_notice() {
    let _settled = ui::motion::testing::settled();
    let store = crate::common::app_store();
    let scratch = Scratch::new("desk-style-trapping");
    let installed = scratch.install("trapping", TRAPPING, store);
    assert_eq!(installed, "atest.trapping", "the style package installs");
    let mut session = session();
    until(&mut session, |session| !session.shell_parts().is_empty());
    until(&mut session, |session| {
        style_state(session, "atest.trapping:trapping").is_some()
    });
    choose(&mut session, "A trapping style of the desk");
    let (_, reason) = crate::common::wait_for(
        &mut session,
        "the trapping style to be marked not loading",
        |session| style_state(session, "atest.trapping:trapping").filter(|(loads, _)| !*loads),
    );
    assert!(
        reason.contains("trap") || reason.contains("divide"),
        "the reason is the trap the view drew: {reason:?}"
    );
    until(&mut session, |session| {
        session
            .notices()
            .log()
            .active()
            .iter()
            .any(|notice| notice.title == "Your desk style could not be shown")
    });
    until(&mut session, |session| {
        let labels = session.accessible_labels();
        labels.iter().any(|label| label == "Dock")
            && labels
                .iter()
                .any(|label| label == "Showing A trapping style of the desk")
    });
}

/// A style that failed and is installed again fixed draws again with no restart: the desk
/// returns to it as soon as its package loads (`shell.md` §6).
#[test]
fn a_fixed_style_package_brings_the_style_back_live() {
    let _settled = ui::motion::testing::settled();
    let store = crate::common::app_store();
    let scratch = Scratch::new("desk-style-fixed");
    let installed = scratch.install("fixed", TRAPPING, store);
    assert_eq!(installed, "atest.trapping", "the style package installs");
    let mut session = session();
    until(&mut session, |session| !session.shell_parts().is_empty());
    until(&mut session, |session| {
        style_state(session, "atest.trapping:trapping").is_some()
    });
    choose(&mut session, "A trapping style of the desk");
    crate::common::wait_for(
        &mut session,
        "the trapping style to be marked not loading",
        |session| style_state(session, "atest.trapping:trapping").filter(|(loads, _)| !*loads),
    );
    // The same package, fixed and installed again: the store moved, the session read it again,
    // and the shell loads the fixed package. The desk returns to the style with no restart.
    let fixed = scratch.install("fixed-again", WORKING, store);
    assert_eq!(
        fixed, "atest.trapping",
        "the fixed package installs as the same app"
    );
    until(&mut session, |session| {
        style_state(session, "atest.trapping:trapping") == Some((true, String::new()))
    });
    until(&mut session, |session| {
        session
            .accessible_labels()
            .iter()
            .any(|label| label.starts_with("A working style"))
    });
}

/// The trapping style's package after its fix: a plain sentence, no trap.
const WORKING: &str = r#"app atest.trapping "A working style"

use so.tilo.shell as shell

/// A working style of the desk
view trapping(a: shell.Arrangement) {
    ui.Text("A working style, {a.places.count()} places")
}
"#;

/// A style chosen by an assistive technology's named press — the route `Session::press_action`,
/// `tilo-control click` and a screen reader's activation take — draws within a frame or two, not
/// at whatever next wakes the shell.
///
/// This fails without the fix: the named press set no input on the shell's part, so the part took
/// the change into its record but never ran its turn for it, and the desk kept drawing the style
/// the person left until the clock's minute woke it (25 s to a minute of wall time in a live
/// session). A press of the pointer sets the part's input and always drew at once, which is why
/// only the named route was slow.
#[test]
fn a_style_chosen_by_a_named_press_draws_within_a_frame_or_two() {
    use ui::window_targets::TargetAction;
    let _settled = ui::motion::testing::settled();
    crate::common::app_store();
    let mut session = session();
    until(&mut session, |session| {
        session
            .accessible_labels()
            .iter()
            .any(|label| label == "Showing Strips")
    });
    // Let the desk rest, so the press is the only thing that may wake it.
    for _ in 0..30 {
        session.step(Duration::from_millis(16)).expect("step");
    }
    // Open the bar's style menu by name, as an assistive technology's user does.
    session
        .press_action("Showing Strips", TargetAction::Activate)
        .expect("the style menu opens");
    for _ in 0..4 {
        session.step(Duration::from_millis(16)).expect("step");
    }
    // Press the Frames row by name, and see what the next frames draw.
    session
        .press_action("Frames", TargetAction::Activate)
        .expect("the Frames row is pressed");
    let mut frames = 0;
    for _ in 0..3 {
        session.step(Duration::from_millis(16)).expect("step");
        frames += 1;
    }
    let labels = session.accessible_labels();
    assert!(
        labels.iter().any(|label| label == "Showing Frames"),
        "the desk shows the style a named press chose, within {frames} frames: {labels:?}"
    );
}

/// Step 4 of shell.md §6, "Two ways to change a desk", without an agent: the desktop style's
/// dock rides the foot, and the style's own action `dock_on` — the one its settings control
/// runs — moves it to the leading edge, and the desk draws the dock there. The settings are the
/// style's own plain tilolang state with defaults, changed by typed actions, and the change
/// applies live.
///
/// This fails without the change: before it the desktop had no settings view of its own and no
/// entry opened one from the desk, so a person could not change where the desktop's dock rides
/// from the place they pick the style.
#[test]
fn the_desktops_dock_edge_setting_moves_the_dock() {
    let _settled = ui::motion::testing::settled();
    common::app_store();
    let root = common::private_home();
    let mut session = common::seeded_session(root.path());
    until(&mut session, |session| {
        common::preset_menu(session).is_some()
    });
    // The desktop, by the style switch's own action: `show_style` names the view the desk draws
    // (`Viewing.style`), through the shell's door as the switch's row runs. The desktop's own
    // bar drops the style menu, so a press on the bar cannot pick it back.
    let mut repl = common::Repl::connect(&root.path().join("agent.sock"));
    repl.opened(&mut session);
    let (answer, failed) = repl.run(
        &mut session,
        "shell.show_style(shell.viewing, shown: \"so.tilo.shell:desktop\")",
    );
    assert!(!failed, "the desk shows the desktop: {answer}");
    until(&mut session, |session| {
        common::preset_menu(session).is_none()
    });
    // The dock rides the foot before the setting: its control stands across the bottom edge.
    let before = common::wait_for(&mut session, "the dock on the foot", |session| {
        common::on_screen(session, "Dock")
    });
    assert!(
        before.1 > f64::from(HEIGHT) / 2.0,
        "the dock rides the foot first: {before:?}"
    );
    // The style's own setting, the action its settings control runs: the dock to the leading
    // edge, through the shell's record the desk reads.
    session
        .run_shell_setting("dock_on", vec![serde_json::json!("left")])
        .unwrap_or_else(|why| panic!("the dock moves to the leading edge: {why}"));
    let after = common::wait_for(&mut session, "the dock on the leading edge", |session| {
        let held = common::on_screen(session, "Dock")?;
        (held.0 < f64::from(WIDTH) / 4.0).then_some(held)
    });
    assert!(
        after.0 < f64::from(WIDTH) / 4.0,
        "the dock stands on the leading edge after the setting: {after:?}"
    );
}

/// Step 4's third part (`shell.md` §6, "Agents make styles with the same commands a person
/// types"): the settings and the switch are ordinary actions the REPL — the person typing what
/// the agent would — calls by the same names, so it switches to the desktop and moves its dock
/// to the leading edge with the calls a Settings control runs, and the desk draws it there.
///
/// This fails without the change: the settings were reached only through the shell's own
/// `run_shell_setting` (a test's back door, not a broker action every caller has), so the REPL
/// could switch the style but not change what the style shows.
#[test]
fn the_repl_moves_the_desktops_dock_by_the_styles_own_action() {
    let _settled = ui::motion::testing::settled();
    common::app_store();
    let root = common::private_home();
    let mut session = common::seeded_session(root.path());
    until(&mut session, |session| {
        common::preset_menu(session).is_some()
    });
    let mut repl = common::Repl::connect(&root.path().join("agent.sock"));
    repl.opened(&mut session);
    // The desktop, through the REPL's own door: the style is a view of the arrangement and the
    // switch is `show_style`, an action of the person's viewing record.
    let (answer, failed) = repl.run(
        &mut session,
        "shell.show_style(shell.viewing, shown: \"so.tilo.shell:desktop\")",
    );
    assert!(!failed, "the REPL shows the desktop: {answer}");
    until(&mut session, |session| {
        common::preset_menu(session).is_none()
    });
    let before = common::wait_for(&mut session, "the dock on the foot", |session| {
        common::on_screen(session, "Dock")
    });
    assert!(
        before.1 > f64::from(HEIGHT) / 2.0,
        "the dock rides the foot: {before:?}"
    );
    // The style's own setting, as the person types what the agent would: the dock to the
    // leading edge.
    let (answer, failed) = repl.run(
        &mut session,
        "shell.dock_on(shell.shell, edge: shell.Edge.left)",
    );
    assert!(!failed, "the REPL moves the dock: {answer}");
    let after = common::wait_for(&mut session, "the dock on the leading edge", |session| {
        let held = common::on_screen(session, "Dock")?;
        (held.0 < f64::from(WIDTH) / 4.0).then_some(held.0)
    });
    assert!(
        after < f64::from(WIDTH) / 4.0,
        "the dock stands on the leading edge after the REPL's call: {after}"
    );
}
