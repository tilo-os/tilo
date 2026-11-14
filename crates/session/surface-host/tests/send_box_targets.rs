//! The send-to box a window opens with no menu around it publishes its controls as the surface's
//! own: what a session names to an agent, presses for an assistive technology, and reads off the
//! desk is the box the person sees, whatever opened it.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test fixtures: a failed step is the assertion"
)]

use tilo_session_core::door::Published as _;
use tilo_session_core::surface::SurfaceHost as _;
use tilo_surface_host::{AppHost, Placement};

/// A session-hosted app window whose own menu carries the send-to box, drawn on the desk.
fn window() -> AppHost {
    let content = ui::widget::Column::new().push(Box::new(ui::widget::Spacer::fixed(200.0)));
    let area = ui::widget::ContextMenuArea::new(Box::new(content), |_| Vec::new());
    let mut host = AppHost::new(
        ui::App::new(ui::State::new(), Box::new(area)),
        Placement {
            origin: (0, 0),
            size: (480, 480),
            scale: 1.0,
        },
    )
    .named("Notes");
    for _ in 0..4 {
        host.prepare_frame();
    }
    host
}

/// The controls the surface publishes now, by name.
fn controls(host: &mut AppHost) -> Vec<(String, Option<String>)> {
    host.controls()
        .into_iter()
        .map(|control| (control.name, control.identity))
        .collect()
}

/// The box the person's send key opens stands in the window with no menu around it, and the
/// surface publishes its parts by name: the field that names whom it goes to, the field that
/// writes the message, and Send.
#[test]
fn the_box_a_send_key_opens_publishes_its_controls() {
    let _settled = ui::motion::testing::settled();
    ui::shared_selection::set_send_host_available(true);
    let _ = ui::shared_selection::take_send_to();
    let mut host = window();
    assert!(
        !controls(&mut host)
            .iter()
            .any(|(name, _)| name == &ui::i18n::widget_text("send-to-send")),
        "no box stands yet: the window's own controls are all it shows"
    );

    ui::shared_selection::open_send_box(None, None, Some("Ship the launch notes"), &[]);
    // The window draws the box on its next frame, and a wake is what brings that frame: the
    // shell's ask reaches an app as a message, which wakes it the same way.
    host.pointer_motion(20.0, 20.0);
    for _ in 0..4 {
        host.prepare_frame();
    }
    assert!(
        ui::shared_selection::take_open_send_box().is_none(),
        "the box asked for was taken by a frame"
    );
    let published = controls(&mut host);
    let names: Vec<&str> = published.iter().map(|(name, _)| name.as_str()).collect();
    for wanted in [
        ui::i18n::widget_text("send-to-recipients-name"),
        ui::i18n::widget_text("send-to-message-name"),
        ui::i18n::widget_text("send-to-send"),
    ] {
        assert!(
            names.contains(&wanted.as_str()),
            "the box's {wanted:?} is published as the window's control: {names:?}"
        );
    }
}

/// Send is pressed through the control the surface publishes, and the app sends what the box
/// holds: the session acts on it as the person, not by aiming a point at the window.
#[test]
fn send_is_pressed_through_the_published_control() {
    let _settled = ui::motion::testing::settled();
    ui::shared_selection::set_send_host_available(true);
    let _ = ui::shared_selection::take_send_to();
    let mut host = window();
    let adalovelace = ui::floating_surface::Recipient::found(
        ui::floating_surface::RecipientKind::Person,
        "person:ada",
        "Ada",
    );
    ui::shared_selection::open_send_box(
        None,
        None,
        Some("Ship the launch notes"),
        std::slice::from_ref(&adalovelace),
    );
    host.pointer_motion(20.0, 20.0);
    for _ in 0..4 {
        host.prepare_frame();
    }
    let send = ui::i18n::widget_text("send-to-send");
    assert!(
        controls(&mut host).iter().any(|(name, _)| name == &send),
        "Send is one of the surface's controls"
    );

    host.press_action(&send, ui::window_targets::TargetAction::Activate)
        .expect("Send answers the press");

    let queue = ui::shared_selection::take_send_to();
    assert_eq!(queue.len(), 1, "one press sends the box once");
    assert_eq!(queue[0].text, "Ship the launch notes");
    assert_eq!(queue[0].to, [adalovelace], "to whom the box was addressed");
}
