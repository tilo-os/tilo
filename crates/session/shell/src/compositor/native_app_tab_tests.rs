//! An app running in its own process steps the tabs of its window group
//! over the real `shell-ui` connection: the app's unconsumed Ctrl+Tab
//! becomes a `cycle_tab` message, the shell's service reads it off the
//! launcher-authenticated socket, and the pump's handler steps the group.

use std::io::Write as _;
use std::os::unix::net::UnixStream;

use winit::keyboard::{Key, NamedKey};

use super::apply_app_cycle_tab;
use crate::compositor::floating_service::{FloatingServiceEvent, FloatingSurfaceService};
use crate::compositor::frame::FrameBuilder;
use crate::shell::state::TileContent;
use ui::floating_surface::{
    FLOATING_SURFACE_SERVICE, FloatingSurfaceClientMessage, FloatingSurfaceServiceClient,
};

const APP: &str = "org.example.TabbedApp";

fn active_tab(frame: &FrameBuilder) -> u64 {
    crate::shell::window_actions::active_tab_id(&frame.chrome()).expect("a focused tab")
}

/// The focused tab shows `APP`, as the window an app in its own process
/// maps there does.
fn focus_app_window(frame: &mut FrameBuilder) -> u64 {
    let focused = active_tab(frame);
    frame.with_chrome(|st| {
        let mut tree = st.tree_mut();
        let tab = tree
            .desktop
            .workspaces
            .iter_mut()
            .flat_map(|workspace| &mut workspace.strips)
            .flat_map(|strip| &mut strip.window_groups)
            .flat_map(|group| &mut group.tabs)
            .find(|tab| tab.id == focused)
            .expect("the focused tab");
        tab.tile = TileContent::Foreign {
            app_id: APP.into(),
            surface_id: 9_001,
        };
    });
    focused
}

#[test]
#[serial_test::serial(process_env)]
fn an_app_in_its_own_process_steps_its_group_with_the_ctrl_tab_it_left_unused() {
    let runtime = tempfile::tempdir().expect("temporary runtime");
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("XDG_RUNTIME_DIR", runtime.path());
    let service = FloatingSurfaceService::start().expect("start the shell-ui service");

    // The launcher authenticates the app's connection before the app runs.
    let mut socket = UnixStream::connect(launcher::services::socket_for(FLOATING_SURFACE_SERVICE))
        .expect("connect");
    let mut handshake = serde_json::to_vec(&serde_json::json!({
        "protocol": launcher::services::SHELL_UI_LAUNCH_PROTOCOL,
        "app_id": APP,
    }))
    .unwrap();
    handshake.push(b'\n');
    socket.write_all(&handshake).unwrap();
    tilo_test_env::wait_until("the app connects", || {
        service
            .drain()
            .into_iter()
            .any(|event| matches!(event, FloatingServiceEvent::Connected { .. }))
    });

    // The app's side of the connection.
    let mut app = FloatingSurfaceServiceClient::from_test_stream(socket).expect("app client");
    let request = ui::shell_tabs::chord_request(&Key::Named(NamedKey::Tab), true, false, false)
        .expect("Ctrl+Tab no widget used is the desktop's next tab");
    let message = request.message(None);

    let mut frame = FrameBuilder::new((1600, 900));
    let start = focus_app_window(&mut frame);
    app.send(&message).expect("send cycle_tab");
    let mut received = None;
    tilo_test_env::wait_until("the shell reads the app's message", || {
        received = service.drain().into_iter().find_map(|event| match event {
            FloatingServiceEvent::Message {
                app_id, message, ..
            } => Some((app_id, message)),
            _ => None,
        });
        received.is_some()
    });
    let (app_id, received) = received.expect("a message");
    assert_eq!(app_id, APP, "the launcher's app id, not one the app claims");
    let FloatingSurfaceClientMessage::CycleTab { forward, instance } = received else {
        panic!("expected cycle_tab, got {received:?}");
    };
    assert!(forward);
    assert!(apply_app_cycle_tab(&mut frame, &app_id, instance, forward));
    let stepped = active_tab(&frame);
    assert_ne!(stepped, start, "the app's group shows its next tab");

    // The tab the app asked from is no longer focused, and another app's tab
    // never was: neither steps anything.
    assert!(!apply_app_cycle_tab(&mut frame, APP, instance, true));
    assert!(!apply_app_cycle_tab(
        &mut frame,
        "org.example.Other",
        None,
        true
    ));
    assert_eq!(active_tab(&frame), stepped);
}
