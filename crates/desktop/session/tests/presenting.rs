//! A laptop plugged into a television, with the shell on both: an app presenting opens a window
//! for the room beside its own. The room's window fills the television and takes no tab; the
//! app's own window covers the laptop as the speaker's view while its tab stays where it was in
//! the strip, and the shell steps aside on both displays. Closing the room's window puts the app's
//! window back in its tab, at its tab's size, and the shell back on both displays.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test fixtures: a failed step is the assertion"
)]

use std::time::Duration;

use crate::common::window::{Client, window_of};
use tilo_compositor::OutputConfig;
use tilo_session::{Options, Session};

const APP: &str = "org.example.Slides";
const AUDIENCE: &str = "org.example.Slides.audience";
const LAPTOP: (i32, i32) = (1000, 700);
const TELEVISION: (i32, i32) = (1200, 800);

/// Keep what `output` shows as `name.png` where `TL_SHELL_SHOTS` names.
fn capture(session: &mut Session, output: &str, name: &str) {
    let Some(dir) = std::env::var_os("TL_SHELL_SHOTS") else {
        return;
    };
    let shot = session
        .compositor()
        .screenshot(output)
        .expect("a screenshot of the display");
    let dir = std::path::PathBuf::from(dir);
    std::fs::create_dir_all(&dir).expect("the captures' directory");
    image::save_buffer(
        dir.join(format!("{name}.png")),
        &shot.rgba,
        shot.width,
        shot.height,
        image::ColorType::Rgba8,
    )
    .expect("the capture written");
}

/// Whether the shell's part on every display steps aside (`true`) or stands (`false`).
fn parts_aside(session: &Session, aside: bool) -> bool {
    let parts = session.shell_parts();
    parts.len() == 2 && parts.iter().all(|part| part.aside == aside)
}

#[test]
fn a_presentation_puts_the_room_on_the_television_and_the_speaker_on_the_laptop() {
    let _settled = tilo_ui_base::motion::testing::settled();
    // The laptop's panel comes first: which screen is the room's is the panel's kind and size.
    let outputs = [
        OutputConfig::new("eDP-1", LAPTOP.0, LAPTOP.1)
            .panel((302, 189))
            .with_scale(1.0)
            .at(0, 0),
        OutputConfig::new("HDMI-A-1", TELEVISION.0, TELEVISION.1)
            .panel((1430, 800))
            .with_scale(1.0)
            .at(LAPTOP.0, 0),
    ];
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            collab: false,
            ..Options::default()
        },
        &outputs,
    )
    .expect("session");
    session.step(Duration::from_millis(1)).unwrap();
    crate::common::wait_for(&mut session, "a shell on both displays", |session| {
        parts_aside(session, false).then_some(())
    });
    let mut client = Client::connect(session.compositor());
    client.pump(&mut session, |_, seen| {
        seen.wm_base.is_some() && seen.shm.is_some()
    });
    let speaker = client.map_plain(&mut session, APP, (400, 300));
    // The app draws at the size its tab gives it before anything is presented.
    let (speaker_id, _) = window_of(session.compositor(), APP).unwrap();
    let tab_size = client.seen.size_at(speaker).expect("configured to its tab");
    client.show(speaker, tab_size.0, tab_size.1, [40, 40, 200, 255]);
    client.pump(&mut session, |session, _| {
        window_of(session.compositor(), APP).is_some_and(|(_, rect)| (rect.2, rect.3) == tab_size)
    });
    let (_, in_tab) = window_of(session.compositor(), APP).unwrap();
    let tab = session
        .desktop()
        .tab_of(speaker_id)
        .expect("the app's window has a tab");
    capture(&mut session, "eDP-1", "presenting-laptop-before");

    // The app presents: its window for the room opens on the same connection.
    let room = client.open_toplevel_with(AUDIENCE, |_| {});
    client.pump(&mut session, |_, seen| seen.ready(room));
    assert_eq!(
        client.seen.size_at(room),
        Some(TELEVISION),
        "the room's window is asked to fill the television"
    );
    client.show(room, TELEVISION.0, TELEVISION.1, [250, 250, 250, 255]);
    client.pump(&mut session, |session, _| {
        session
            .compositor()
            .presenting()
            .is_some_and(|presenting| presenting.presenter.is_some())
    });
    let presenting = session.compositor().presenting().unwrap();
    let (room_id, _) = window_of(session.compositor(), AUDIENCE).unwrap();
    assert_eq!(presenting.audience, room_id);
    assert_eq!(presenting.audience_on, "HDMI-A-1");
    assert_eq!(presenting.presenter, Some((speaker_id, "eDP-1".to_owned())));
    assert_eq!(
        session.desktop().tab_of(room_id),
        None,
        "the room's window takes no place in the strips"
    );

    let covered = client.redraw_when_resized(&mut session, speaker, (in_tab.2, in_tab.3));
    assert_eq!(covered, LAPTOP, "the speaker's view is sized to the laptop");
    client.pump(&mut session, |session, _| {
        window_of(session.compositor(), APP) == Some((speaker_id, (0, 0, LAPTOP.0, LAPTOP.1)))
            && window_of(session.compositor(), AUDIENCE)
                == Some((room_id, (LAPTOP.0, 0, TELEVISION.0, TELEVISION.1)))
            && parts_aside(session, true)
    });
    assert_eq!(
        session.desktop().tab_of(speaker_id),
        Some(tab),
        "the app's window keeps its tab while it covers the laptop"
    );
    capture(&mut session, "eDP-1", "presenting-laptop");
    capture(&mut session, "HDMI-A-1", "presenting-television");

    // The talk ends: the room's window goes, and the app's window is back in its tab.
    client.close(room);
    let back = client.redraw_when_resized(&mut session, speaker, LAPTOP);
    assert_eq!(back, (in_tab.2, in_tab.3), "back at its tab's size");
    client.pump(&mut session, |session, _| {
        window_of(session.compositor(), APP) == Some((speaker_id, in_tab))
            && parts_aside(session, false)
    });
    assert_eq!(session.compositor().presenting(), None);
    capture(&mut session, "eDP-1", "presenting-laptop-after");
}
