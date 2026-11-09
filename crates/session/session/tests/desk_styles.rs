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

mod common;

use std::time::Duration;

use tilo_compositor::{KeyRepeat, OutputConfig};
use tilo_session::{Options, Service as _, Session};
use tilo_surface_host::DeskReading;
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
    common::until(session, "the shell's answer", done);
}

/// Choose `style` as the desk's style, as the switch does: one `show_style` on the person's
/// viewing, naming it by its view reference.
fn choose(session: &mut Session, style: &str) {
    session
        .invoke(
            "so.tilo.shell",
            "show_style",
            &serde_json::Map::from_iter([(
                "shown".to_owned(),
                serde_json::Value::String(style.to_owned()),
            )]),
        )
        .unwrap_or_else(|error| panic!("choosing {style}: {error:?}"));
}

/// Step 2 of shell.md §6, "Styles are views": the four styles `so.tilo.shell` ships are views of
/// the arrangement, published on the desk's reading and listed by their doc lines, and the desk
/// draws the one the person chooses through `show`. Switching keeps the arrangement.
#[test]
fn the_four_shipped_styles_are_listed_and_drawn() {
    let _settled = ui::motion::testing::settled();
    common::app_store();
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
    choose(&mut session, "so.tilo.shell:frames");
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
    let store = common::app_store();
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
        r#"app a-test.timeline "A timeline"

use so.tilo.shell as shell

/// Timeline of the desk
view timeline(a: shell.Arrangement) {
    ui.Text("Timeline of {a.places.count()} places")
}
"#,
    )
    .expect("the style package");
    let installed = common::dev_apps::install_package(&source, store);
    assert_eq!(installed, "a-test.timeline", "the style package installs");

    let mut session = session();
    until(&mut session, |session| !session.shell_parts().is_empty());
    // The store moved, so the session reads it again: the styles it lists now hold the person's
    // own beside the shell's four.
    until(&mut session, |session| {
        desk(session)
            .styles
            .iter()
            .any(|style| style.style == "a-test.timeline:timeline")
    });
    let styles = desk(&session).styles;
    let own = styles
        .iter()
        .find(|style| style.style == "a-test.timeline:timeline")
        .expect("the person's style is listed");
    assert!(own.loads, "the person's style loads: {own:?}");
    assert_eq!(
        own.line, "Timeline of the desk",
        "listed by its author's doc line"
    );
    // Choosing it is one action on the person's viewing, as the switch runs, and the desk draws
    // the package's own view of the arrangement: its words stand on the screen.
    choose(&mut session, "a-test.timeline:timeline");
    until(&mut session, |session| {
        session
            .accessible_labels()
            .iter()
            .any(|label| label.starts_with("Timeline of "))
    });
    std::fs::remove_dir_all(&scratch).ok();
}
