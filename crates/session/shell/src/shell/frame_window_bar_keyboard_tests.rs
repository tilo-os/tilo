//! The keyboard of a window whose app the shell mounts in its own process (a
//! Terminal tab): a press on one of the app's own slots in its bar is a press
//! on the app, so the keyboard stays with the control in it that had it; a
//! press on the app takes the keyboard, and then every key is the app's.

use super::*;

use automerge::{AutoCommit, ROOT};
use ui::window_toolbar::{ToolbarAction, WindowToolbar};
use winit::event::{ElementState, MouseButton};
use winit::keyboard::{ModifiersState, NamedKey};

fn key(text: &str) -> WindowEvent {
    WindowEvent::KeyboardInput {
        device_id: winit::event::DeviceId::dummy(),
        event: ui::synthetic_input::key_event(
            Key::Character(text.into()),
            Some(text.into()),
            ElementState::Pressed,
        ),
        is_synthetic: false,
    }
}

/// A window alone on the desk showing a mounted Terminal with a real
/// process behind it, whose app put an action in the window's bar on the
/// window the shell lent it. Returns the frame and what reaches the process.
fn terminal_with_a_bar_action() -> (FrameBuilder, std::sync::mpsc::Receiver<Vec<u8>>) {
    let mut fb = FrameBuilder::new((1600, 900));
    let tab = crate::shell::state::next_id();
    fb.with_chrome(|st| {
        let mut tree = st.tree_mut();
        let workspace = tree.desktop.active_workspace_mut().expect("a workspace");
        let strip = workspace.strips.first_mut().expect("a strip");
        let group = crate::shell::state::next_id();
        // Alone in the strip, so its bar is on screen.
        strip.window_groups.clear();
        strip.window_groups.push(crate::shell::state::WindowGroup {
            id: group,
            tabs: vec![crate::shell::state::Tab {
                id: tab,
                title: "Terminal".into(),
                tile: crate::shell::state::TileContent::AppPlaceholder {
                    kind: crate::shell::state::AppKind::Terminal,
                    label: "Terminal".into(),
                },
                app_title: None,
            }],
            active_tab: tab,
            width: crate::shell::state::WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        });
        let strip_id = strip.id;
        tree.desktop.set_active_group(strip_id, group);
        drop(tree);
        st.bump();
    });
    fb.relayout();

    // A real process behind the mounted Terminal hears what reaches it.
    let id = fb.active_terminal_node().expect("the mounted Terminal");
    // SAFETY: this pointer belongs to the current mounted owner.
    let terminal = hosted_terminal(unsafe { &mut **fb.node_map.get(&id).unwrap() })
        .expect("the shell mounts Terminal in its own process");
    *terminal = terminal::TerminalWidget::spawn_command(terminal::CommandBuilder::new("cat"))
        .expect("attach a PTY process to the Terminal");
    let probe = terminal.attach_input_probe();
    fb.rebuild_layout_tree();

    // The app puts an action in its window's bar, on the window the shell
    // lent it.
    let window = {
        let chrome = fb.chrome();
        let apps = chrome
            .hosted_inputs
            .in_process()
            .expect("the shell mounts apps in its own process")
            .clone();
        drop(chrome);
        apps.borrow_mut().hosted_window(tab)
    };
    window.declare(Some(WindowToolbar {
        actions: vec![ToolbarAction::new("panel", "pin", "Toggle panel")],
        ..WindowToolbar::default()
    }));
    for _ in 0..3 {
        let mut doc = AutoCommit::new();
        fb.pump_apps(&mut doc, &ROOT);
        fb.relayout();
        let _ = fb.paint();
    }
    (fb, probe)
}

#[test]
fn a_press_on_an_in_process_apps_bar_slot_leaves_typing_with_the_app() {
    let (mut fb, probe) = terminal_with_a_bar_action();
    let slot = fb
        .widget_registry()
        .get("window_action:0")
        .copied()
        .filter(|rect| rect.area() > 0.0)
        .expect("the app's action stands in its window's bar");

    let mut doc = AutoCommit::new();
    let mut focus = None;
    let mut send = |fb: &mut FrameBuilder, event: &WindowEvent, focus: &mut Option<NodeId>| {
        fb.dispatch_event(
            event,
            &Modifiers::default(),
            &mut ui::widget::Edits::lend(&mut doc),
            &ROOT,
            focus,
        )
    };
    send(&mut fb, &key("a"), &mut focus);
    assert_eq!(
        focus,
        fb.active_terminal_node(),
        "the Terminal has the keyboard"
    );
    assert_eq!(
        tilo_test_env::recv("what was typed into the Terminal", &probe),
        b"a".to_vec()
    );

    let center = slot.center();
    for event in [
        ui::synthetic_input::cursor_moved(center.x, center.y),
        ui::synthetic_input::mouse_input(ElementState::Pressed, MouseButton::Left),
        ui::synthetic_input::mouse_input(ElementState::Released, MouseButton::Left),
    ] {
        send(&mut fb, &event, &mut focus);
    }
    // The Terminal reads its window's events itself (and has no use for
    // this action); what matters here is where the keyboard went: still
    // the Terminal's, not the bar's.
    assert_eq!(
        focus,
        fb.active_terminal_node(),
        "the bar does not take the keyboard from the app"
    );
    send(&mut fb, &key("b"), &mut focus);
    assert_eq!(
        tilo_test_env::recv("what was typed after the press", &probe),
        b"b".to_vec(),
        "typing after a press on the app's slot still reaches the app"
    );

    // End the typed line, then close the process's input so it and its
    // reader end (an end of input mid-line only flushes the line).
    let id = fb.active_terminal_node().expect("the mounted Terminal");
    // SAFETY: the live map was rebuilt after attaching its process.
    let terminal = hosted_terminal(unsafe { &mut **fb.node_map.get(&id).unwrap() })
        .expect("the Terminal stays mounted");
    terminal
        .try_write_input(b"\n\x04")
        .expect("close the process's input");
    tilo_test_env::wait_until("the PTY process and reader finish", || {
        terminal.is_disconnected()
    });
}

/// While the Terminal has the keyboard, Tab and Shift+Tab are its shell's as
/// every other key is: they complete and back-tab, and focus stays on the
/// Terminal. A press on it gives it the keyboard wherever the desktop's
/// focus was left.
#[test]
fn a_terminal_with_the_keyboard_takes_tab_and_shift_tab() {
    let (mut fb, probe) = terminal_with_a_bar_action();
    let terminal = fb.active_terminal_node().expect("the mounted Terminal");
    let chrome = fb
        .focusable_order()
        .into_iter()
        .find(|node| *node != terminal)
        .expect("a desktop control to leave focus on");
    // SAFETY: the live map was rebuilt after attaching its process.
    let centre = unsafe { (**fb.node_map.get(&terminal).unwrap()).get_rect() }.center();

    let mut doc = AutoCommit::new();
    let mut focus = None;
    fb.set_native_focus(&mut focus, Some(chrome));
    let mut send = |fb: &mut FrameBuilder,
                    event: &WindowEvent,
                    modifiers: ModifiersState,
                    focus: &mut Option<NodeId>| {
        fb.dispatch_event(
            event,
            &Modifiers::from(modifiers),
            &mut ui::widget::Edits::lend(&mut doc),
            &ROOT,
            focus,
        )
    };
    for event in [
        ui::synthetic_input::cursor_moved(centre.x, centre.y),
        ui::synthetic_input::mouse_input(ElementState::Pressed, MouseButton::Left),
        ui::synthetic_input::mouse_input(ElementState::Released, MouseButton::Left),
    ] {
        send(&mut fb, &event, ModifiersState::empty(), &mut focus);
    }
    assert_eq!(
        focus,
        Some(terminal),
        "a press on the Terminal takes the keyboard"
    );
    while probe.try_recv().is_ok() {}

    let tab = ui::synthetic_input::named_key(NamedKey::Tab);
    for (modifiers, expected) in [
        (ModifiersState::empty(), b"\t".to_vec()),
        (ModifiersState::SHIFT, b"\x1b[Z".to_vec()),
        (ModifiersState::empty(), b"\t".to_vec()),
    ] {
        assert!(
            send(&mut fb, &tab, modifiers, &mut focus),
            "Tab with {modifiers:?} is the Terminal's"
        );
        assert_eq!(
            tilo_test_env::recv("what Tab sent the Terminal's process", &probe),
            expected,
            "Tab with {modifiers:?} reaches the shell"
        );
        assert_eq!(focus, Some(terminal), "the Terminal keeps the keyboard");
    }

    // SAFETY: the live map was rebuilt after attaching its process.
    let terminal = hosted_terminal(unsafe { &mut **fb.node_map.get(&terminal).unwrap() })
        .expect("the Terminal stays mounted");
    // End the line the Tabs left open, then close the process's input.
    terminal
        .try_write_input(b"\n\x04")
        .expect("close the process's input");
    tilo_test_env::wait_until("the PTY process and reader finish", || {
        terminal.is_disconnected()
    });
}
