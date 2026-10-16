use super::*;
use crate::widget::{Column, TextInput};

fn drawn(field: TextInput) -> App {
    let mut app = App::new_with_controlled_inputs(
        State::new(),
        Box::new(Column::new().push(Box::new(field))),
    );
    app.init_layout();
    let root = app.layout.root_node.unwrap();
    run_layout_pass(&mut app, root, 600, 400, 1.0, false, None).unwrap();
    draw_frame_content(&mut app, &mut Scene::new(), 600, 400, 1.0, None);
    app
}

#[test]
fn a_window_showing_a_typed_secret_is_withheld_from_captures() {
    // Case `a_typed_password_withholds_the_window_masked_or_shown`.
    for revealed in [false, true] {
        let mut field = TextInput::local()
            .with_secret(true)
            .with_initial_value("hunter2");
        if revealed {
            field.toggle_reveal();
        }
        let app = drawn(field);
        assert!(!app.capture_protected(), "the app asked for nothing");
        assert!(app.wants_capture_protection(), "revealed: {revealed}");
    }

    // Case `an_empty_password_or_plain_text_withholds_nothing`.
    for field in [
        TextInput::local().with_secret(true),
        TextInput::local().with_initial_value("hunter2"),
    ] {
        assert!(!drawn(field).wants_capture_protection());
    }

    // Case `an_app_that_asked_stays_withheld`.
    let mut app = drawn(TextInput::local());
    app.set_capture_protected(true);
    assert!(app.wants_capture_protection());
}

#[test]
fn empty_revealed_secret_preedit_protects_the_production_frame() {
    use winit::event::Ime;
    let mut field = TextInput::local().with_secret(true);
    field.toggle_reveal();
    field.set_focus(true);
    let handled = crate::testing::dispatch(
        &mut field,
        &WindowEvent::Ime(Ime::Preedit("秘密".into(), Some((6, 6)))),
    );
    assert!(handled);
    assert_eq!(field.value(), "", "composition is not committed text");
    assert!(drawn(field).wants_capture_protection());
}

#[cfg(unix)]
#[test]
fn capture_submission_waits_for_the_matching_transport_ack_and_latches_until_teardown() {
    use crate::floating_surface::{
        FloatingSurfaceClientMessage, FloatingSurfaceHostMessage, FloatingSurfaceServiceClient,
    };
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;
    let (app_side, mut shell_side) = UnixStream::pair().unwrap();
    shell_side
        .set_read_timeout(Some(tilo_test_env::hang_bound()))
        .unwrap();
    let mut reader = BufReader::new(shell_side.try_clone().unwrap());
    let mut app = drawn(
        TextInput::local()
            .with_secret(true)
            .with_initial_value("secret"),
    );
    app.floating = crate::app::FloatingRuntime::new(Some(
        FloatingSurfaceServiceClient::from_stream(app_side).unwrap(),
    ));
    app.set_secret_on_screen(true);
    crate::webview::paint::set_share_watched(true);
    crate::webview::paint::queue_share(crate::webview::paint::SharedFrame {
        payload: b"secret page payload".to_vec(),
        page: [0.0, 0.0, 600.0, 400.0],
        scale: 1.0,
    });
    assert!(
        !app.capture_frame_ready(),
        "sending does not establish protection"
    );
    assert!(
        crate::webview::paint::take_share_frames().is_empty(),
        "the production transport discards the protected frame's queued paint stream before any ack"
    );
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let generation = match serde_json::from_str::<FloatingSurfaceClientMessage>(&line).unwrap() {
        FloatingSurfaceClientMessage::SetCaptureProtection {
            protected: true,
            generation: Some(generation),
        } => generation,
        other => panic!("unexpected request: {other:?}"),
    };
    let acknowledge = |shell: &mut UnixStream, generation, protected| {
        let mut bytes = serde_json::to_vec(&FloatingSurfaceHostMessage::CaptureProtectionApplied {
            generation,
            protected,
        })
        .unwrap();
        bytes.push(b'\n');
        shell.write_all(&bytes).unwrap();
    };
    acknowledge(&mut shell_side, generation.saturating_sub(1), true);
    assert!(
        !app.capture_frame_ready(),
        "a stale acknowledgement authorises no pixels"
    );
    acknowledge(&mut shell_side, generation, true);
    tilo_test_env::wait_until("capture policy acknowledgement", || {
        app.capture_frame_ready()
    });
    app.capture_frame_submitted(1);
    app.set_secret_on_screen(false);
    assert!(
        app.wants_capture_protection(),
        "secret render work is still queued"
    );
    app.capture_frame_presented(1);
    assert!(
        app.wants_capture_protection(),
        "the displayed frame still has the secret"
    );
    app.capture_frame_submitted(2);
    assert!(
        app.wants_capture_protection(),
        "submitting a clear is not presenting it"
    );
    app.capture_frame_presented(2);
    assert!(
        app.wants_capture_protection(),
        "worker completion cannot prove a compositor buffer replacement"
    );
    let latched_generation = app.floating.capture_generation;
    for sequence in 3..6 {
        app.capture_frame_submitted(sequence);
        app.capture_frame_presented(sequence);
        app.pump_floating_surface_service();
        assert!(
            app.wants_capture_protection(),
            "repeated benign frames retain the native lifetime latch"
        );
        assert_eq!(
            app.floating.capture_generation, latched_generation,
            "no unprotect request was emitted"
        );
    }
    crate::webview::paint::set_share_watched(false);
}

#[test]
fn auxiliary_secret_frames_keep_the_conservative_native_lifetime_latch() {
    let mut app = drawn(TextInput::local());
    app.capture_auxiliary_drawn("detached", true);
    assert!(app.wants_capture_protection());
    app.capture_auxiliary_submitted("detached");
    app.capture_auxiliary_drawn("detached", false);
    assert!(app.wants_capture_protection());
    app.capture_auxiliary_completed("detached", true);
    assert!(app.wants_capture_protection());
    app.capture_auxiliary_submitted("detached");
    app.capture_auxiliary_completed("detached", false);
    assert!(
        app.wants_capture_protection(),
        "a skipped replacement does not clear displayed pixels"
    );
    app.capture_auxiliary_submitted("detached");
    app.capture_auxiliary_completed("detached", true);
    assert!(
        app.wants_capture_protection(),
        "auxiliary destruction is not acknowledged by this protocol; retain app-wide protection until teardown"
    );
}
