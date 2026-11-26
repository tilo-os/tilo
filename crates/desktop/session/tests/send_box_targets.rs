//! The send-to box a window opens with no menu around it publishes its controls as the surface's
//! own: what a session names to an agent, presses for an assistive technology, and reads off the
//! desktop is the box the person sees, whatever opened it.

#![cfg(target_os = "linux")]

use tilo_session::platform::surfaces::{AppProcess, Placement};
use tilo_session_core::surface::ShellSurface as _;
use tilo_session_core::surfaces::Published as _;

/// A session-hosted app window with no menu of its own, drawn on the desktop: the window carries
/// the send-to box itself.
fn window() -> AppProcess {
    let content = ui::widget::Column::new().push(Box::new(tilo_ui_base::widget::Spacer::fixed(200.0)));
    let mut host = AppProcess::new(
        ui::App::new(ui::State::new(), Box::new(content)),
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
fn controls(host: &mut AppProcess) -> Vec<(String, Option<String>)> {
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
    let _settled = tilo_ui_base::motion::testing::settled();
    ui::shared_selection::set_send_host_available(true);
    let _ = ui::shared_selection::take_send_to();
    let mut host = window();
    assert!(
        !controls(&mut host)
            .iter()
            .any(|(name, _)| name == &tilo_ui_base::i18n::widget_text("send-to-send")),
        "no box stands yet: the window's own controls are all it shows"
    );

    host.app_mut()
        .open_send_box(None, None, Some("Ship the launch notes"), &[], &[]);
    for _ in 0..4 {
        host.prepare_frame();
    }
    let published = controls(&mut host);
    let names: Vec<&str> = published.iter().map(|(name, _)| name.as_str()).collect();
    for wanted in [
        tilo_ui_base::i18n::widget_text("send-to-recipients-name"),
        tilo_ui_base::i18n::widget_text("send-to-message-name"),
        tilo_ui_base::i18n::widget_text("send-to-send"),
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
    let _settled = tilo_ui_base::motion::testing::settled();
    ui::shared_selection::set_send_host_available(true);
    let _ = ui::shared_selection::take_send_to();
    let mut host = window();
    let adalovelace = ui::floating_surface::Recipient::found(
        ui::floating_surface::RecipientKind::Person,
        "ada",
        "Ada",
    );
    host.app_mut().open_send_box(
        None,
        None,
        Some("Ship the launch notes"),
        std::slice::from_ref(&adalovelace),
        &[],
    );
    for _ in 0..4 {
        host.prepare_frame();
    }
    let send = tilo_ui_base::i18n::widget_text("send-to-send");
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
