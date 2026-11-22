//! The window in front wears the person's accent on its own edge: Settings' Appearance page says
//! the accent colours the focus ring, and the window the keyboard went to shows it. The pick
//! reaches the window that already stands on the desk, without a restart.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and a session \
              outlives every call made into it"
)]

use std::time::Duration;

use serde_json::{Map, Value, json};
use tilo_compositor::Screenshot;
use tilo_desktop::verbs::CHROME_RESOURCE;
use tilo_session::{Options, Session};

/// The person's appearance section, as the settings service publishes it.
const APPEARANCE: &str = "settings:settings.appearance";
/// Who the person is to others, with the colour they wear, as the settings service publishes it.
const IDENTITY: &str = "settings:settings.identity";
/// One display's refresh: how often the session asks its clients for their next frame.
const REFRESH: Duration = Duration::from_millis(16);

/// A session as a development one starts: the seeded apps published, the person's settings service
/// beside them, and the machine's app lists, so the launcher opens the apps it holds.
fn session() -> Session {
    let mut services = crate::common::seeded_apps();
    services.push(Box::new(tilo_settings_service::SettingsService::new()));
    crate::common::headless(Options {
        person: crate::common::PERSON.to_owned(),
        state: Some(crate::common::device_state()),
        shell: false,
        collab: false,
        lists: tilo_session::Lists::fixed(tilo_session::AppLists::read()),
        services,
        ..Options::default()
    })
}

fn step(session: &mut Session) {
    session.step(REFRESH).expect("a turn of the session");
}

/// Open `app` on the desk the way the launcher does, and answer its window's tab once it is drawn.
fn open(session: &mut Session, app: &str) -> u64 {
    let arguments = json!({ "app": app });
    session
        .invoke(
            CHROME_RESOURCE,
            "open-app",
            arguments.as_object().expect("an object"),
        )
        .unwrap_or_else(|error| panic!("{app} opens on the desk: {error:?}"));
    crate::common::until(session, &format!("{app}'s window"), |session| {
        tab_of(session, app).is_some()
    });
    step(session);
    tab_of(session, app).expect("the app's window is on the desk")
}

/// The tab of `app`'s window, while the desk holds one.
fn tab_of(session: &Session, app: &str) -> Option<u64> {
    session.handle().read(|tree| {
        tree.workspaces
            .iter()
            .flat_map(|workspace| &workspace.strips)
            .flat_map(|strip| &strip.window_groups)
            .flat_map(|group| &group.tabs)
            .find(|tab| {
                tilo_desktop_model::apps::canonical_app_id(&tab.tile)
                    .is_some_and(|shown| shown == app)
            })
            .map(|tab| tab.id)
    })
}

/// Where the window `tab` is drawn on the display.
fn rect(session: &Session, tab: u64) -> ui::kurbo::Rect {
    session
        .handle()
        .window_rect(tab)
        .unwrap_or_else(|| panic!("the window {tab} is drawn"))
}

/// Set the field `field` of the appearance section through its own published `field-edit`, as
/// Settings' Appearance page and an agent both do.
fn set_field(session: &mut Session, field: &str, value: &Value) {
    set_field_of(session, APPEARANCE, field, value);
}

/// Set the field `field` of the settings section `section` through its own `field-edit`.
fn set_field_of(session: &mut Session, section: &str, field: &str, value: &Value) {
    let arguments: Map<String, Value> = json!({
        "edit": {"path": [{"field": field}], "op": "set", "value": value}
    })
    .as_object()
    .expect("an object")
    .clone();
    session
        .invoke(section, "field-edit", &arguments)
        .unwrap_or_else(|error| panic!("set {field}: {error:?}"));
}

/// The window in front's edge is the person's own colour while they picked no accent, the accent
/// they pick once they do, and it follows a pick under the window it already stands around.
#[test]
fn the_window_in_fronts_edge_follows_the_accent_the_person_picks() {
    let _home = tilo_test_env::TestHome::new().expect("a home of its own");
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    let notes = open(&mut session, "so.tilo.notes");
    for _ in 0..40 {
        step(&mut session);
    }

    // With no accent picked, the theme's own is the person's colour, as Settings shows it and every
    // app draws it: the shell's ring is that colour too, never a default of its own.
    let own = ui::tokens::PresenceHue::Olive;
    set_field_of(&mut session, IDENTITY, "colour", &json!(own.id()));
    for _ in 0..40 {
        step(&mut session);
    }
    let shot = frame(&mut session);
    let colour = ui::color::Oklch::from_color(ring_at(&shot, rect(&session, notes)));
    assert!(
        hue_distance(colour.h, own.degrees()) <= 15.0,
        "with no accent picked, the window in front's edge is the person's own colour ({}°); it is \
         at {}° (chroma {:.3})",
        own.degrees(),
        colour.h,
        colour.c
    );

    let mut seen = Vec::new();
    for (accent, hue) in [("teal", 175.0), ("pink", 355.0)] {
        set_field(&mut session, "accent", &json!(accent));
        for _ in 0..40 {
            step(&mut session);
        }
        let shot = frame(&mut session);
        let colour = ui::color::Oklch::from_color(ring_at(&shot, rect(&session, notes)));
        assert!(
            hue_distance(colour.h, hue) <= 15.0,
            "the window in front's edge is the {accent} accent ({hue}°), the focus ring's kind; \
             it is at {}° (chroma {:.3})",
            colour.h,
            colour.c
        );
        seen.push(colour.h);
    }
    assert!(
        hue_distance(seen[0], seen[1]) > 40.0,
        "the edge moves with the accent: {seen:?}"
    );
}

/// The frame the session draws.
fn frame(session: &mut Session) -> Screenshot {
    session
        .compositor()
        .screenshot("HEADLESS-1")
        .expect("the session draws its display")
}

/// The most coloured pixel of the band the window's edge paints just outside `rect`, halfway down
/// its left side.
fn ring_at(shot: &Screenshot, rect: ui::kurbo::Rect) -> ui::peniko::Color {
    let x = rect.x0.round() as i32;
    let y = (rect.y0 + rect.height() / 2.0).round() as i32;
    (-3..=0)
        .filter_map(|across| {
            let x = u32::try_from(x + across).ok()?;
            let y = u32::try_from(y).ok()?;
            shot.pixel(x, y)
        })
        .map(|[r, g, b, a]| ui::peniko::Color::from_rgba8(r, g, b, a))
        .max_by(|a, b| {
            ui::color::Oklch::from_color(*a)
                .c
                .total_cmp(&ui::color::Oklch::from_color(*b).c)
        })
        .expect("the window's edge stands on the display")
}

/// The distance between two hues, in degrees, the short way round the ring.
fn hue_distance(a: f32, b: f32) -> f32 {
    let d = (a - b).rem_euclid(360.0);
    d.min(360.0 - d)
}
