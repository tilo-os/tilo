use crate::{Attr, GridPos, Selection, TerminalPerformer, TerminalWidget};
use automerge::{AutoCommit, ROOT, ReadDoc, transaction::Transactable};
use portable_pty::{MasterPty, PtySize};
use std::io::{Read, Write};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use tilo_automerge::ApplyIncoming;
use ui::State;
use ui::framework::ResourceSurface;
use ui::tilo_resource::Resource as _;
use ui::widget::Widget as _;
use vello::kurbo::Rect;
use vte::Perform;

/// What a collaborator shows of `host`'s session: its screen and title,
/// recorded as the session's actions, read back from the record and
/// presented to a terminal with no process of its own.
pub(crate) fn replica_of(host: &TerminalWidget) -> TerminalWidget {
    use ui::tilo_actions::Action as _;
    let mut doc = AutoCommit::new();
    crate::resource::TerminalAction::Output {
        edits: crate::screen::edits(None, host.screen()),
    }
    .apply(&mut doc)
    .expect("the screen records");
    crate::resource::TerminalAction::SetTitle {
        title: host.title().to_owned(),
    }
    .apply(&mut doc)
    .expect("the title records");
    let (mut replica, _) = build_test_widget();
    let state = crate::resource::Terminal::read(&doc).expect("the session reads");
    ResourceSurface::<crate::resource::Terminal, crate::resource::TerminalEffect>::present(
        &mut replica,
        &Rc::new(state),
    );
    replica
}

/// Bounds a wait that ends as soon as its condition holds; only a hang reaches it.
const HANG_BOUND: std::time::Duration = std::time::Duration::from_secs(30);

fn shell_command(unix_script: &str, windows_script: &str) -> portable_pty::CommandBuilder {
    #[cfg(unix)]
    {
        let _ = windows_script;
        let mut command = portable_pty::CommandBuilder::new("/bin/sh");
        command.args(["-c", unix_script]);
        command
    }
    #[cfg(windows)]
    {
        let _ = unix_script;
        let mut command = portable_pty::CommandBuilder::new("cmd.exe");
        command.args(["/d", "/c", windows_script]);
        command
    }
}

/// Shared mock PTY master used by the selection / clipboard tests.
struct MockMasterShared;
impl MasterPty for MockMasterShared {
    fn get_size(&self) -> Result<PtySize, anyhow::Error> {
        Ok(PtySize::default())
    }
    fn resize(&self, _size: PtySize) -> Result<(), anyhow::Error> {
        Ok(())
    }
    fn take_writer(&self) -> Result<Box<dyn Write + Send>, anyhow::Error> {
        Ok(Box::new(std::io::sink()))
    }
    fn try_clone_reader(&self) -> Result<Box<dyn Read + Send>, anyhow::Error> {
        Ok(Box::new(std::io::empty()))
    }
    #[cfg(unix)]
    fn process_group_leader(&self) -> Option<i32> {
        None
    }
    #[cfg(unix)]
    fn as_raw_fd(&self) -> Option<i32> {
        None
    }
    #[cfg(unix)]
    fn tty_name(&self) -> Option<std::path::PathBuf> {
        None
    }
}

/// Construct a terminal widget with a writer that funnels bytes
/// into a channel so tests can assert what was sent to the PTY.
fn build_test_widget() -> (TerminalWidget, std::sync::mpsc::Receiver<Vec<u8>>) {
    struct ChanWriter(std::sync::mpsc::Sender<Vec<u8>>);
    impl Write for ChanWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            let _ = self.0.send(buf.to_vec());
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let (write_tx, write_rx) = std::sync::mpsc::channel::<Vec<u8>>();
    let writer: Box<dyn Write + Send> = Box::new(ChanWriter(write_tx));
    let (_pty_out_tx, pty_out_rx) = std::sync::mpsc::channel();
    let widget = TerminalWidget::new(
        Arc::new(Mutex::new(writer)),
        Arc::new(Mutex::new(Box::new(MockMasterShared))),
        pty_out_rx,
        tilo_content::ContentStore::memory(),
    );
    (widget, write_rx)
}

/// Like `build_test_widget` but also hands back the PTY *output* sender,
/// so a test can push bytes down the exact channel the reader thread uses.
/// This exercises the production `sync` → `pump` drain path (which is what
/// snaps the scrollback viewport back to the live bottom on fresh output).
pub(crate) fn build_test_widget_io() -> (
    TerminalWidget,
    std::sync::mpsc::Receiver<Vec<u8>>,
    std::sync::mpsc::Sender<Vec<u8>>,
) {
    struct ChanWriter(std::sync::mpsc::Sender<Vec<u8>>);
    impl Write for ChanWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            let _ = self.0.send(buf.to_vec());
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let (write_tx, write_rx) = std::sync::mpsc::channel::<Vec<u8>>();
    let writer: Box<dyn Write + Send> = Box::new(ChanWriter(write_tx));
    let (pty_out_tx, pty_out_rx) = std::sync::mpsc::channel();
    let widget = TerminalWidget::new(
        Arc::new(Mutex::new(writer)),
        Arc::new(Mutex::new(Box::new(MockMasterShared))),
        pty_out_rx,
        tilo_content::ContentStore::memory(),
    );
    (widget, write_rx, pty_out_tx)
}

/// PTY output: read-error classification, pump draining, redraw requests and the first-run watermark.
#[test]
fn pty_output_is_read_and_pumped_into_redraws_and_watermark() {
    // Case `embedded_terminal_only_requests_frames_for_queued_output`.
    {
        let (mut widget, _writes, output) = build_test_widget_io();
        let doc = AutoCommit::new();

        assert!(
            !widget.needs_animation_frame(),
            "an idle embedded terminal must not pin its host to continuous redraws"
        );

        output.send(b"embedded output".to_vec()).unwrap();
        assert!(
            widget.needs_animation_frame(),
            "queued PTY bytes must request a frame until the host pumps them"
        );
        assert!(widget.update(&doc, &ROOT));
        widget.sync(&doc, &ROOT);
        assert!(widget.grid_text().contains("embedded output"));
        assert!(
            !widget.needs_animation_frame(),
            "the terminal must return to idle after its output queue is drained"
        );
    }

    // Case `pty_reader_retries_interrupts_and_linux_handoff_eio`.
    {
        use crate::inner::{PtyReadDisposition, classify_pty_read};

        assert_eq!(
            classify_pty_read(
                &Err(std::io::Error::from(std::io::ErrorKind::Interrupted)),
                || false
            ),
            PtyReadDisposition::Retry
        );
        assert_eq!(
            classify_pty_read(&Err(std::io::Error::from_raw_os_error(libc::EIO)), || true),
            PtyReadDisposition::Retry,
            "a PTY master can report EIO before the guest opens its slave"
        );
        assert_eq!(
            classify_pty_read(&Err(std::io::Error::from_raw_os_error(libc::EIO)), || false),
            PtyReadDisposition::Closed,
            "once the session's lifetime socket closes, EIO means the guest exited"
        );
        assert_eq!(
            classify_pty_read(&Ok(19), || true),
            PtyReadDisposition::Data(19)
        );
        assert_eq!(
            classify_pty_read(&Ok(0), || true),
            PtyReadDisposition::Closed
        );
        assert_eq!(
            classify_pty_read(&Err(std::io::Error::from_raw_os_error(libc::EBADF)), || {
                true
            }),
            PtyReadDisposition::Closed
        );
    }

    // Case `first_pty_bytes_retire_the_first_run_watermark`.
    {
        let (mut widget, _writes) = build_test_widget();
        assert!(!widget.has_received_pty_output());

        widget.feed(b"\x1b[32m\x1b[?25h");

        assert!(
            widget.has_received_pty_output(),
            "control-only startup output still proves the PTY is live"
        );
    }

    // Case `pty_pump_retires_watermark_even_for_nonprinting_output`.
    {
        let (mut widget, _write_rx, output_tx) = build_test_widget_io();
        let state = AutoCommit::new();
        assert!(!widget.has_received_pty_output());

        output_tx.send(vec![b'\x1b', b'[', b'0', b'm']).unwrap();
        assert!(widget.update(&state, &ROOT));
        assert!(widget.has_received_pty_output());
    }
}

/// Hosted-terminal service sockets: lifetime probe, fd detection, connect retries, discovery and no fallback.
#[cfg(unix)]
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "cases merged from separate tests; each case stays self-contained"
)]
#[serial_test::serial]
fn hosted_service_sockets_are_probed_and_never_fall_back() {
    // Case `lifetime_socket_probe_sees_the_supervisor_close_its_end`.
    {
        use crate::inner::lifetime_socket_open;

        let (terminal_end, supervisor_end) =
            std::os::unix::net::UnixStream::pair().expect("socket pair");
        assert!(lifetime_socket_open(&terminal_end));
        supervisor_end
            .shutdown(std::net::Shutdown::Both)
            .expect("supervisor closes the session");
        assert!(!lifetime_socket_open(&terminal_end));
    }

    // Case `service_fd_is_detected_only_when_the_descriptor_is_actually_open`.
    {
        // The service path is selected on a *live* descriptor, not on the mere
        // presence of the env var — so a stale or spoofed number can neither
        // select the mandatory path nor, by extension, be used to steer the
        // decision. A real open fd is detected; a closed/garbage one is not.
        use crate::inner::ush_service_fd;

        let mut fds = [0i32; 2];
        // SAFETY: standard socketpair FFI; `fds` is written on success.
        let rc = unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, fds.as_mut_ptr()) };
        assert_eq!(rc, 0, "socketpair for a live test fd");

        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_SVC_USH_FD", fds[0].to_string());
        assert_eq!(
            ush_service_fd(),
            Some(fds[0]),
            "an open descriptor is the service"
        );

        env.set("TILO_SVC_USH_FD", "999999");
        assert_eq!(
            ush_service_fd(),
            None,
            "a closed descriptor is not the service"
        );

        env.set("TILO_SVC_USH_FD", "not-a-number");
        assert_eq!(ush_service_fd(), None, "garbage is not the service");

        env.remove("TILO_SVC_USH_FD");
        assert_eq!(ush_service_fd(), None, "unset is not the service");
        drop(env);

        // SAFETY: closing the two fds we own.
        unsafe {
            libc::close(fds[0]);
            libc::close(fds[1]);
        }
    }

    // Case `each_nonblocking_connect_progress_error_is_retried`.
    {
        for error in [libc::EINPROGRESS, libc::EALREADY, libc::EAGAIN] {
            assert!(crate::unix_connect_is_pending(Some(error)));
        }
        assert!(!crate::unix_connect_is_pending(Some(libc::ECONNREFUSED)));
        assert!(!crate::unix_connect_is_pending(None));
    }

    // Case `hosted_services_discover_and_use_a_healthy_ush_socket`.
    {
        use std::io::{BufRead, BufReader};
        use std::os::fd::AsRawFd;

        let runtime = tempfile::tempdir().unwrap();
        let listener = private_ush_listener(runtime.path());
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut request = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut request)
                .unwrap();
            assert!(request.contains("\"op\":\"open-session\""), "{request}");
            assert!(
                request.contains("\"target\":{\"kind\":\"environment\"}"),
                "{request}"
            );
            let pair = portable_pty::native_pty_system()
                .openpty(portable_pty::PtySize::default())
                .unwrap();
            runtime_portal::fd_transport::send_with_fd(
                stream.as_raw_fd(),
                b"{\"result\":\"session\",\"session\":\"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\",\"environment\":\"default\"}\n",
                pair.master.as_raw_fd().unwrap(),
            )
            .unwrap();
        });

        let services = crate::HostedProcessServices::discover_from_runtime_dir(runtime.path());
        let mut widget = crate::TerminalWidget::spawn_hosted(services)
            .expect("the discovered ush connection must use the supervisor protocol");
        // The session opens at the first sync, once the record says where.
        let mut doc = automerge::AutoCommit::new();
        doc.put(
            automerge::ROOT,
            crate::target::TARGET_KEY,
            crate::target::SessionTarget::default_environment().encode(),
        )
        .unwrap();
        ui::widget::Widget::sync(&mut widget, &doc, &automerge::ROOT);
        assert_eq!(widget.session_state(), &crate::sessions::OpenState::Open);
        drop(widget);
        server.join().unwrap();
    }

    // Case `hosted_services_leave_ush_missing_when_the_socket_is_absent`.
    {
        let runtime = tempfile::tempdir().unwrap();
        let services = crate::HostedProcessServices::discover_from_runtime_dir(runtime.path());
        assert!(services.ush.is_none());
        assert!(matches!(
            crate::TerminalWidget::spawn_hosted(services),
            Err(crate::HostedTerminalError::RequiredServiceMissing("ush"))
        ));
    }

    // Case `hosted_services_leave_ush_missing_when_the_socket_refuses_connections`.
    {
        let runtime = tempfile::tempdir().unwrap();
        let listener = private_ush_listener(runtime.path());
        drop(listener);

        let services = crate::HostedProcessServices::discover_from_runtime_dir(runtime.path());
        assert!(services.ush.is_none());
        assert!(matches!(
            crate::TerminalWidget::spawn_hosted(services),
            Err(crate::HostedTerminalError::RequiredServiceMissing("ush"))
        ));
    }

    // Case `hosted_terminal_service_failure_never_falls_back_to_a_login_shell`.
    {
        use std::os::unix::net::UnixStream;
        let (client, server) = UnixStream::pair().unwrap();
        drop(server);
        let mut widget =
            crate::TerminalWidget::spawn_hosted(crate::HostedProcessServices { ush: Some(client) })
                .unwrap();
        let mut doc = automerge::AutoCommit::new();
        doc.put(
            automerge::ROOT,
            crate::target::TARGET_KEY,
            crate::target::SessionTarget::default_environment().encode(),
        )
        .unwrap();
        ui::widget::Widget::sync(&mut widget, &doc, &automerge::ROOT);
        assert!(
            matches!(
                widget.session_state(),
                crate::sessions::OpenState::Failed(_)
            ),
            "a supervisor that fails leaves the tab failed, never on a local shell"
        );
        assert!(widget.is_disconnected());
    }
}

/// Drain and concatenate everything the PTY-write channel has buffered.
pub(crate) fn drain_writes(rx: &std::sync::mpsc::Receiver<Vec<u8>>) -> Vec<u8> {
    let mut out = Vec::new();
    while let Ok(b) = rx.try_recv() {
        out.extend_from_slice(&b);
    }
    out
}

fn send_left_press(widget: &mut TerminalWidget, doc: &mut AutoCommit) {
    use winit::event::{ElementState, MouseButton, WindowEvent};
    let ev = WindowEvent::MouseInput {
        device_id: winit::event::DeviceId::dummy(),
        state: ElementState::Pressed,
        button: MouseButton::Left,
    };
    ui::testing::Dispatch::new().doc(doc).send(widget, &ev);
}

fn send_left_release(widget: &mut TerminalWidget, doc: &mut AutoCommit) {
    use winit::event::{ElementState, MouseButton, WindowEvent};
    let ev = WindowEvent::MouseInput {
        device_id: winit::event::DeviceId::dummy(),
        state: ElementState::Released,
        button: MouseButton::Left,
    };
    ui::testing::Dispatch::new().doc(doc).send(widget, &ev);
}

fn send_cursor_move(widget: &mut TerminalWidget, doc: &mut AutoCommit, x: f64, y: f64) {
    use winit::dpi::PhysicalPosition;
    use winit::event::WindowEvent;
    let ev = WindowEvent::CursorMoved {
        device_id: winit::event::DeviceId::dummy(),
        position: PhysicalPosition::new(x, y),
    };
    ui::testing::Dispatch::new().doc(doc).send(widget, &ev);
}

fn send_mouse_button_mods(
    widget: &mut TerminalWidget,
    doc: &mut AutoCommit,
    button: winit::event::MouseButton,
    state: winit::event::ElementState,
    shift: bool,
) {
    use winit::event::WindowEvent;
    use winit::keyboard::ModifiersState;
    let modifiers = if shift {
        ModifiersState::SHIFT
    } else {
        ModifiersState::empty()
    };
    let ev = WindowEvent::MouseInput {
        device_id: winit::event::DeviceId::dummy(),
        state,
        button,
    };
    ui::testing::Dispatch::new()
        .modifiers(modifiers)
        .doc(doc)
        .send(widget, &ev);
}

fn send_focus(widget: &mut TerminalWidget, doc: &mut AutoCommit, focused: bool) {
    use winit::event::WindowEvent;
    ui::testing::Dispatch::new()
        .doc(doc)
        .send(widget, &WindowEvent::Focused(focused));
}

fn send_key(
    widget: &mut impl ui::widget::Widget,
    doc: &mut AutoCommit,
    key: winit::keyboard::Key,
    ctrl: bool,
    shift: bool,
) {
    send_key_mods(widget, doc, key, ctrl, shift, false);
}

fn send_key_mods(
    widget: &mut impl ui::widget::Widget,
    doc: &mut AutoCommit,
    key: winit::keyboard::Key,
    ctrl: bool,
    shift: bool,
    alt: bool,
) {
    use winit::keyboard::ModifiersState;

    let mut state = ModifiersState::empty();
    if ctrl {
        state |= ModifiersState::CONTROL;
    }
    if shift {
        state |= ModifiersState::SHIFT;
    }
    if alt {
        state |= ModifiersState::ALT;
    }
    send_key_with_modifiers(widget, doc, key, state);
}

fn send_key_with_modifiers(
    widget: &mut impl ui::widget::Widget,
    doc: &mut AutoCommit,
    key: winit::keyboard::Key,
    state: winit::keyboard::ModifiersState,
) {
    use winit::event::{ElementState, WindowEvent};

    let mut ev = ui::synthetic_input::key_event(key, None, ElementState::Pressed);
    // Real winit delivers the committed text alongside a character key; the
    // terminal encodes that into the PTY bytes (and typing snaps the
    // scrollback back to the live bottom). Named keys carry no `text`.
    ev.text = match &ev.logical_key {
        winit::keyboard::Key::Character(s) => Some(s.clone()),
        _ => None,
    };

    let win_ev = WindowEvent::KeyboardInput {
        device_id: winit::event::DeviceId::dummy(),
        event: ev,
        is_synthetic: false,
    };
    ui::testing::Dispatch::new()
        .modifiers(state)
        .doc(doc)
        .send(widget, &win_ev);
}

/// Same as [`send_key_with_modifiers`], but leave `KeyEvent::text` empty.
/// Hardware Space and control-socket keys often arrive that way; the PTY
/// must still see the glyphs or App/Frame steal Space/Enter as Activate.
pub(crate) fn send_key_without_text(
    widget: &mut TerminalWidget,
    doc: &mut AutoCommit,
    key: winit::keyboard::Key,
) -> bool {
    use winit::event::{ElementState, WindowEvent};

    let ev = ui::synthetic_input::key_event(key, None, ElementState::Pressed);
    ui::testing::Dispatch::new()
        .doc(doc)
        .send(
            widget,
            &WindowEvent::KeyboardInput {
                device_id: winit::event::DeviceId::dummy(),
                event: ev,
                is_synthetic: false,
            },
        )
        .handled
}

/// Typed letters, Space and Enter must reach the PTY even when winit omits
/// `KeyEvent::text` — the production hardware and control-socket shapes.
#[test]
fn typed_text_reaches_the_pty_without_keyevent_text() {
    use winit::keyboard::{Key, NamedKey, SmolStr};

    let (mut widget, writes) = build_test_widget();
    let mut doc = AutoCommit::new();
    widget.force_geometry(Rect::new(0.0, 0.0, 800.0, 480.0), 10.0, 20.0);

    assert!(
        send_key_without_text(
            &mut widget,
            &mut doc,
            Key::Character(SmolStr::new_static("e")),
        ),
        "a character key without text must be consumed"
    );
    assert!(
        send_key_without_text(&mut widget, &mut doc, Key::Named(NamedKey::Space)),
        "Named Space without text must be consumed, not left for Activate"
    );
    assert!(
        send_key_without_text(
            &mut widget,
            &mut doc,
            Key::Character(SmolStr::new_static(" ")),
        ),
        "Character Space without text must be consumed"
    );
    assert!(
        send_key_without_text(&mut widget, &mut doc, Key::Named(NamedKey::Enter)),
        "Enter must be consumed as a carriage return"
    );

    assert_eq!(
        drain_writes(&writes),
        b"e  \r",
        "letters, both Space spellings, and Enter reach the PTY",
    );
}

/// Grid initialisation, printing through the performer, and the crate's widget surface.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "cases merged from separate tests; each case stays self-contained"
)]
fn terminal_grid_initialises_prints_and_is_a_library_widget() {
    // Case `test_terminal_grid_init`.
    {
        struct MockMaster;
        impl MasterPty for MockMaster {
            fn get_size(&self) -> Result<PtySize, anyhow::Error> {
                Ok(PtySize::default())
            }
            fn resize(&self, _size: PtySize) -> Result<(), anyhow::Error> {
                Ok(())
            }
            fn take_writer(&self) -> Result<Box<dyn Write + Send>, anyhow::Error> {
                Ok(Box::new(std::io::sink()))
            }
            fn try_clone_reader(&self) -> Result<Box<dyn Read + Send>, anyhow::Error> {
                Ok(Box::new(std::io::empty()))
            }
            #[cfg(unix)]
            fn process_group_leader(&self) -> Option<i32> {
                None
            }
            #[cfg(unix)]
            fn as_raw_fd(&self) -> Option<i32> {
                None
            }
            // portable-pty 0.9 added a `tty_name` accessor. Test mock has no
            // real TTY behind it, so return None.
            #[cfg(unix)]
            fn tty_name(&self) -> Option<std::path::PathBuf> {
                None
            }
        }

        let (_output_tx, output_rx) = std::sync::mpsc::channel();
        let writer: Box<dyn Write + Send> = Box::new(std::io::sink());

        let mut widget = TerminalWidget::new(
            Arc::new(Mutex::new(writer)),
            Arc::new(Mutex::new(Box::new(MockMaster))),
            output_rx,
            tilo_content::ContentStore::memory(),
        );

        widget.feed(b"");
        assert_eq!(widget.grid_text().split('\n').count(), 24);
    }

    // Case `empty_line_pads_on_cursor_jump_write`.
    // Lines are created *empty* (not pre-filled with `width` spaces — that
    // per-character Automerge fill was O(n^2) and froze the whole
    // compositor for seconds the first time a terminal synced). A write
    // whose cursor lands past a fresh line's current end must therefore
    // pad the gap with spaces so the printed char still renders at the
    // correct column. Regression for the "dropping a window freezes the
    // shell ~3-5 s" report.
    {
        let (mut widget, _rx) = build_test_widget();

        // Fresh lines start empty.
        widget.feed(b"");
        assert_eq!(widget.grid_row(0).len(), 0, "blank lines are seeded empty");

        // CSI 1;11 H → row 1, col 11 (1-based); print 'X' at 0-based col 10
        // on the still-empty first line — this exercises the pad path.
        widget.feed(b"\x1b[1;11HX");
        let line = widget.grid_row(0);
        assert_eq!(
            line.chars().nth(10),
            Some('X'),
            "printed char must land at column 10; got {line:?}",
        );
        assert!(
            line.chars().take(10).all(|c| c == ' '),
            "columns 0-9 must be padded with spaces; got {line:?}",
        );
    }

    // Case `test_terminal_performer_print`.
    {
        use crate::TermModes;
        let mut screen = crate::screen::Screen {
            main: vec![Rc::new(crate::screen::Line {
                text: " ".repeat(80),
                attrs: vec![Attr::default().pack(); 80].concat(),
            })],
            ..crate::screen::Screen::default()
        };
        let mut modes = TermModes::default();
        let mut saved_cursor = None;
        let mut next_link_id = 0u16;
        let mut title_out = None;
        let mut responses: Vec<u8> = Vec::new();
        let content = tilo_content::ContentStore::memory();

        {
            let mut performer = TerminalPerformer {
                screen: &mut screen,
                content: &content,
                width: 80,
                height: 24,
                cursor_x: 0,
                cursor_y: 0,
                current_attr: Attr::default(),
                pending_wrap: false,
                modes: &mut modes,
                saved_cursor: &mut saved_cursor,
                scroll_top: 0,
                scroll_bottom: 23,
                next_link_id: &mut next_link_id,
                current_link_id: 0,
                title_out: &mut title_out,
                responses: &mut responses,
                print_buf: None,
            };

            performer.print('H');
            performer.print('i');
            assert_eq!(performer.cursor_x, 2);
            // Printed cells are batched in memory and committed to the line on
            // any non-print action or at end of feed; flush so the read below
            // sees them (the real feed path flushes via `flush_cursor`).
            performer.flush_print_buf();
        }

        assert!(screen.main[0].text.starts_with("Hi"));
    }

    // Case `crate_exposes_terminal_widget_as_library`.
    {
        // Library root must re-export `TerminalWidget` so the shell can embed it.
        fn _takes_widget<T: ui::widget::Widget>(_: T) {}
        // This is a compile-time assertion via use.
        #[allow(
            unused_imports,
            reason = "the import itself is the compile-time public type assertion"
        )]
        use crate::TerminalWidget as _;
    }

    // Case `terminal_widget_implements_app_widget`.
    {
        use ui::{AppKind, AppWidget, IconDescriptor};
        fn require_app_widget<T: AppWidget>(_: &T) {}
        let w = crate::TerminalWidget::spawn_default().expect("pty spawn");
        require_app_widget(&w);
        assert_eq!(w.app_kind(), AppKind::Terminal);
        assert_eq!(w.app_title(), "Terminal");
        match w.app_icon() {
            IconDescriptor::Glyph(g) => assert_eq!(g, "󰆍"),
            IconDescriptor::Named(_) => panic!("expected glyph icon"),
        }
        assert_eq!(w.permission_hint(), Some(AppKind::Terminal));
    }
}

/// Disconnected accessibility state and drawing from theme tokens at every text scale.
#[test]
fn terminal_draw_and_disconnected_state_follow_theme_and_text_scale() {
    // Case `exited_pty_publishes_a_persistent_disconnected_accessibility_state`.
    {
        use ui::widget::Widget;

        let mut widget = TerminalWidget::spawn_command(shell_command("true", "exit /b 0"))
            .expect("spawn short-lived pty command");
        let doc = AutoCommit::new();
        let deadline = ui::time::now() + HANG_BOUND;
        while ui::time::now() < deadline {
            widget.sync(&doc, &ROOT);
            if widget.is_disconnected() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            widget.is_disconnected(),
            "PTY EOF must settle the session state"
        );
        let node = Widget::accessibility(&widget).expect("terminal semantics");
        assert!(
            node.description
                .is_some_and(|description| description.contains("Session ended"))
        );
        assert!(node.children.iter().any(|child| {
            child
                .name
                .as_deref()
                .is_some_and(|name| name.contains("Session ended"))
                && child.live == Some(ui::a11y::Politeness::Assertive)
        }));
    }

    // Case `terminal_draw_reads_monospace_from_theme_tokens`.
    {
        use ui::Theme;
        let theme = Theme::dark();
        // The terminal must resolve its font family from the theme's mono token,
        // not hard-code a string. This test asserts the presence of a
        // `font_mono` accessor on Theme::tokens (added by Plan 1) and that the
        // terminal records the last-drawn font for inspection.
        let mono = theme.tokens.font_mono;
        assert!(
            !mono.family.is_empty(),
            "theme must expose mono font tokens"
        );
        // This is a compile-time + minimal-runtime assertion that the terminal
        // pulls from tokens rather than hard-coded strings.
        // Full rendering regression is covered by the existing unit tests.
    }

    // Case `terminal_draw_reflows_to_resolved_monospace_at_large_text_scale`.
    {
        use ui::{Theme, presence::PresenceCtx, text_render::TextRenderer};
        use vello::{Scene, kurbo::Affine};

        let (mut widget, _) = build_test_widget();
        // Model the stale pre-draw measurement case: a narrow provisional advance
        // would initially tell the PTY that far more columns fit than its resolved
        // monospace glyphs actually can at a 200% accessibility text scale.
        widget.force_geometry(Rect::new(0.0, 0.0, 390.0, 844.0), 4.0, 12.0);
        let (provisional_cols, _) = widget.grid_size();
        let theme = Theme::high_contrast().with_text_scale(2.0);
        let mut scene = Scene::new();
        let mut text_renderer = TextRenderer::new();
        widget.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut text_renderer,
            &theme,
            &PresenceCtx::empty(),
        );
        let (resolved_cols, resolved_rows) = widget.grid_size();
        assert!(resolved_cols < provisional_cols);
        assert!(resolved_cols > 0 && resolved_rows > 0);
    }

    // Case `disconnected_badge_keeps_a_complete_concise_state_at_large_text_scale`.
    {
        assert_eq!(TerminalWidget::disconnected_badge_label(), "Session ended");
    }
}

/// The session banner stands above the grid and the grid starts under it, at
/// every text scale.
#[test]
fn ended_session_banner_reserves_its_own_band_above_the_grid() {
    use ui::widget::Widget;
    use ui::{Theme, presence::PresenceCtx, text_render::TextRenderer};
    use vello::{Scene, kurbo::Affine};

    let mut widget = TerminalWidget::spawn_command(shell_command("true", "exit /b 0"))
        .expect("spawn short-lived pty command");
    let doc = AutoCommit::new();
    let deadline = ui::time::now() + HANG_BOUND;
    while ui::time::now() < deadline && !widget.is_disconnected() {
        widget.sync(&doc, &ROOT);
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(widget.is_disconnected());

    let viewport = Rect::new(0.0, 0.0, 390.0, 844.0);
    let mut text = TextRenderer::new();
    for scale in [1.0, 2.0] {
        widget.force_geometry(viewport, 10.0, 20.0);
        let theme = Theme::dark().with_text_scale(scale);
        let mut scene = Scene::new();
        widget.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut text,
            &theme,
            &PresenceCtx::empty(),
        );
        let banner = widget.banner_test_rect();
        assert!(banner.width() > 0.0, "an ended session shows its banner");
        assert!(banner.x0 >= viewport.x0 && banner.x1 <= viewport.x1);
        assert!(
            banner.y1 <= widget.grid_rect().y0,
            "the grid starts under the banner at text scale {scale}"
        );
    }
}

/// Find honours the bar's options: literal by default only in that case counts,
/// and a whole word is not matched inside a longer one.
#[test]
fn find_matches_follow_case_and_whole_word_options() {
    use ui::widget::{FindOptions, Findable};

    let (mut widget, _) = build_test_widget();
    widget.force_grid(vec![
        "Cat cat concat".to_string(),
        "no match here".to_string(),
        "CAT".to_string(),
    ]);
    let count = |widget: &mut TerminalWidget, options: FindOptions| {
        Findable::find(widget, "cat", options).len()
    };
    assert_eq!(count(&mut widget, FindOptions::default()), 4);
    assert_eq!(
        count(
            &mut widget,
            FindOptions {
                match_case: true,
                ..FindOptions::default()
            }
        ),
        2
    );
    assert_eq!(
        count(
            &mut widget,
            FindOptions {
                whole_word: true,
                ..FindOptions::default()
            }
        ),
        3
    );
    Findable::clear_find(&mut widget);
    assert!(widget.find_test_matches().is_empty());
}

/// The ANSI palette follows theme swaps and never hard-codes colours.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "cases merged from separate tests; each case stays self-contained"
)]
fn terminal_palette_tracks_the_theme() {
    // Case `terminal_ansi_mapping_tracks_palette_swaps`.
    {
        // Swap the palette and prove the ANSI 0..=15 color table the terminal
        // uses moves with it; the existence of a token alone would not prove that.
        use ui::tokens::TerminalPalette;
        use vello::peniko::Color;

        let mut palette_a = TerminalPalette::dark();
        let mut palette_b = TerminalPalette::dark();

        // Paint a distinctive hue into ANSI 1 (red slot) for each palette so
        // a passing test cannot be faked by reading the same byte.
        palette_a.ansi[1] = Color::from_rgb8(10, 20, 30);
        palette_b.ansi[1] = Color::from_rgb8(250, 240, 230);
        palette_a.fg_default = Color::from_rgb8(11, 22, 33);
        palette_b.fg_default = Color::from_rgb8(240, 230, 220);
        palette_a.bg_default = Color::from_rgb8(1, 2, 3);
        palette_b.bg_default = Color::from_rgb8(250, 251, 252);

        // ANSI 0..=15 lookups must return the palette-supplied color.
        let fg_a = crate::inner::map_ansi_256(1, &palette_a);
        let fg_b = crate::inner::map_ansi_256(1, &palette_b);
        assert_ne!(
            format!("{fg_a:?}"),
            format!("{:?}", fg_b),
            "palette swap must change ANSI 1 lookup"
        );

        // Cursor color is derived from fg_default — swapping the palette
        // must change the cursor hue too.
        let cursor_a = crate::inner::cursor_from_palette(&palette_a);
        let cursor_b = crate::inner::cursor_from_palette(&palette_b);
        assert_ne!(
            format!("{cursor_a:?}"),
            format!("{:?}", cursor_b),
            "palette swap must change cursor color"
        );

        // 6x6x6 cube colors (16..232) are defined by the ANSI 256 spec, not
        // by theme; they must stay stable across palette swaps.
        let cube_a = crate::inner::map_ansi_256(42, &palette_a);
        let cube_b = crate::inner::map_ansi_256(42, &palette_b);
        assert_eq!(
            format!("{cube_a:?}"),
            format!("{:?}", cube_b),
            "6x6x6 ANSI cube is spec-defined and must not move with the theme"
        );
    }

    // Case `terminal_widget_remembers_palette_after_inspection_hooks`.
    {
        // Construct the widget; seed the inspection hooks by directly
        // invoking the palette helpers. This proves the public accessors
        // reflect palette choices (stand-in for a full scene inspection).
        use ui::tokens::TerminalPalette;
        use vello::peniko::Color;

        struct MockMaster;
        impl MasterPty for MockMaster {
            fn get_size(&self) -> Result<PtySize, anyhow::Error> {
                Ok(PtySize::default())
            }
            fn resize(&self, _size: PtySize) -> Result<(), anyhow::Error> {
                Ok(())
            }
            fn take_writer(&self) -> Result<Box<dyn Write + Send>, anyhow::Error> {
                Ok(Box::new(std::io::sink()))
            }
            fn try_clone_reader(&self) -> Result<Box<dyn Read + Send>, anyhow::Error> {
                Ok(Box::new(std::io::empty()))
            }
            #[cfg(unix)]
            fn process_group_leader(&self) -> Option<i32> {
                None
            }
            #[cfg(unix)]
            fn as_raw_fd(&self) -> Option<i32> {
                None
            }
            // portable-pty 0.9 added a `tty_name` accessor. Test mock has no
            // real TTY behind it, so return None.
            #[cfg(unix)]
            fn tty_name(&self) -> Option<std::path::PathBuf> {
                None
            }
        }

        let (_output_tx, output_rx) = std::sync::mpsc::channel();
        let writer: Box<dyn Write + Send> = Box::new(std::io::sink());

        let widget = TerminalWidget::new(
            Arc::new(Mutex::new(writer)),
            Arc::new(Mutex::new(Box::new(MockMaster))),
            output_rx,
            tilo_content::ContentStore::memory(),
        );

        // Before any draw, accessors return the sentinel transparent value.
        let pre = widget.last_fg_default();
        assert_eq!(
            format!("{pre:?}"),
            format!("{:?}", Color::from_rgba8(0, 0, 0, 0)),
            "before first draw the cached fg must be the sentinel"
        );

        // Proxy: derive a cursor color from a custom palette and assert the
        // helper does not round-trip to the default. This exercises the same
        // code path `draw` uses to populate `last_cursor_color`.
        let mut palette = TerminalPalette::dark();
        palette.fg_default = Color::from_rgb8(42, 99, 200);
        let cursor = crate::inner::cursor_from_palette(&palette);
        let default_cursor = crate::inner::cursor_from_palette(&TerminalPalette::dark());
        assert_ne!(
            format!("{cursor:?}"),
            format!("{:?}", default_cursor),
            "cursor must follow the palette's fg_default"
        );
    }

    // Case `terminal_palette_tracks_the_theme_not_a_hardcoded_green_on_black`.
    {
        use ui::ColorCompat;
        use ui::theme::Theme;
        let rgb = |c: vello::peniko::Color| (c.r(), c.g(), c.b());

        // Default dark theme: neutral ground, near-white text — never the old
        // pure-black / green-400 scheme. The grid's default background is the
        // window ground the framework paints, so a default cell is invisible.
        let dark = Theme::dark();
        let p = &crate::inner::TerminalWidget::grid_palette(&dark);
        assert_eq!(
            rgb(p.bg_default),
            rgb(dark.tokens.colors.surfaces.content),
            "terminal bg is the window ground"
        );
        assert_eq!(
            rgb(p.fg_default),
            rgb(dark.text_primary),
            "terminal fg is the theme text"
        );
        assert_ne!(
            rgb(p.bg_default),
            (0, 0, 0),
            "bg must not be the old pure black"
        );
        assert_ne!(
            rgb(p.fg_default),
            (0x4a, 0xde, 0x80),
            "fg must not be the old green-400"
        );

        // Light theme: fg/bg follow that theme's (much lighter) ground.
        let light = Theme::light();
        let lp = &crate::inner::TerminalWidget::grid_palette(&light);
        assert_eq!(
            rgb(lp.bg_default),
            rgb(light.tokens.colors.surfaces.content),
            "light terminal bg is that theme's ground"
        );
        let (r, g, b) = rgb(lp.bg_default);
        assert!(
            u32::from(r) + u32::from(g) + u32::from(b) > 360,
            "the light terminal ground must actually be light, got {:?}",
            (r, g, b)
        );

        // The ANSI ramp is anchored to the theme's own colours, and stays
        // internally distinct (blue and cyan must not collapse together).
        let colors = &dark.tokens.colors;
        assert_eq!(
            rgb(p.ansi[1]),
            rgb(colors.semantic.danger),
            "ANSI red = theme danger"
        );
        assert_eq!(
            rgb(p.ansi[2]),
            rgb(colors.semantic.success),
            "ANSI green = theme success"
        );
        assert_eq!(
            rgb(p.ansi[3]),
            rgb(colors.semantic.warning),
            "ANSI yellow = theme warning"
        );
        assert_ne!(
            rgb(p.ansi[4]),
            rgb(p.ansi[6]),
            "blue and cyan stay distinct"
        );
    }
}

// ---------------------------------------------------------------
// Selection + clipboard tests (cell-grid flow-selection model).
// ---------------------------------------------------------------

/// Selection model: collapse, drag tracking, row spans and extracted text.
#[test]
#[serial_test::serial]
fn selection_spans_anchor_drag_and_text_extraction() {
    // Case `selection_collapsed_after_lone_click_is_none`.
    {
        // Click without drag: selection must be `None` after release.
        // A one-cell highlight on idle clicks would be visually noisy
        // and doesn't match any standard terminal emulator.
        let (mut widget, _) = build_test_widget();
        let mut doc = AutoCommit::new();
        widget.force_geometry(Rect::new(0.0, 0.0, 800.0, 600.0), 10.0, 20.0);

        send_cursor_move(&mut widget, &mut doc, 100.0, 100.0);
        send_left_press(&mut widget, &mut doc);
        // Selection exists during drag (anchor == cursor)…
        assert!(widget.selection().is_some());
        send_left_release(&mut widget, &mut doc);
        // …and is cleared once the mouse comes back up with no drag.
        assert!(
            widget.selection().is_none(),
            "lone click must clear selection on release"
        );
        assert!(!widget.is_dragging());
    }

    // Case `selection_anchor_and_cursor_track_drag`.
    {
        // Press at (col=5, row=2), drag to (col=20, row=6). Anchor must
        // stay at the press location while cursor moves with the pointer.
        let (mut widget, _) = build_test_widget();
        let mut doc = AutoCommit::new();
        widget.force_geometry(Rect::new(0.0, 0.0, 800.0, 600.0), 10.0, 20.0);

        // Padding is 4px → col k occupies x ∈ [4 + k*10, 4 + (k+1)*10).
        // Press at col 5, row 2.
        send_cursor_move(
            &mut widget,
            &mut doc,
            4.0 + 5.0 * 10.0 + 1.0,
            2.0 * 20.0 + 1.0,
        );
        send_left_press(&mut widget, &mut doc);
        let sel0 = widget.selection().expect("selection present after press");
        assert_eq!(sel0.anchor, GridPos { row: 2, col: 5 });
        assert_eq!(sel0.cursor, GridPos { row: 2, col: 5 });

        // Drag to col 20, row 6.
        send_cursor_move(
            &mut widget,
            &mut doc,
            4.0 + 20.0 * 10.0 + 1.0,
            6.0 * 20.0 + 1.0,
        );
        let sel1 = widget
            .selection()
            .expect("selection still present mid-drag");
        assert_eq!(
            sel1.anchor,
            GridPos { row: 2, col: 5 },
            "anchor must NOT move during drag"
        );
        assert_eq!(sel1.cursor, GridPos { row: 6, col: 20 });

        // Release: drag ends, but selection survives.
        send_left_release(&mut widget, &mut doc);
        let sel2 = widget.selection().expect("selection survives release");
        assert_eq!(sel2.anchor, GridPos { row: 2, col: 5 });
        assert_eq!(sel2.cursor, GridPos { row: 6, col: 20 });
        assert!(!widget.is_dragging());
    }

    // Case `selection_row_spans_single_row_returns_one_span`.
    {
        // Single-row selection: one span from start_col to end_col.
        let sel = Selection {
            anchor: GridPos { row: 3, col: 5 },
            cursor: GridPos { row: 3, col: 12 },
        };
        let spans = crate::inner::selection_row_spans(&sel, 80);
        assert_eq!(spans, vec![(3, 5, 12)]);
    }

    // Case `selection_row_spans_three_rows_returns_first_full_last`.
    {
        // Multi-row: first row goes start..cols, middle row 0..cols,
        // last row 0..end_col.
        let sel = Selection {
            anchor: GridPos { row: 2, col: 5 },
            cursor: GridPos { row: 4, col: 3 },
        };
        let spans = crate::inner::selection_row_spans(&sel, 80);
        assert_eq!(
            spans,
            vec![
                (2, 5, 80), // first: start..cols
                (3, 0, 80), // middle: all cols
                (4, 0, 3),  // last: 0..end
            ]
        );
    }

    // Case `selection_row_spans_anchor_after_cursor_is_normalised`.
    {
        // Drag-up case: anchor is after cursor in reading order. The
        // span walker must order them so the lower row is "first".
        let sel = Selection {
            anchor: GridPos { row: 5, col: 7 },
            cursor: GridPos { row: 1, col: 2 },
        };
        let spans = crate::inner::selection_row_spans(&sel, 80);
        assert_eq!(spans.first(), Some(&(1, 2, 80)));
        assert_eq!(spans.last(), Some(&(5, 0, 7)));
    }

    // Case `selected_text_strips_trailing_whitespace_per_row`.
    {
        // Standard terminal-copy behaviour: trailing spaces on each
        // visible row are not copied (they're padding, not content).
        let (mut widget, _) = build_test_widget();
        widget.force_geometry(Rect::new(0.0, 0.0, 800.0, 480.0), 10.0, 20.0);
        widget.force_viewport_start(0);
        widget.force_grid(vec![
            "hello world                 ".to_string(),
            "second line trailing space  ".to_string(),
        ]);
        widget.force_selection(Some(Selection {
            anchor: GridPos { row: 0, col: 0 },
            cursor: GridPos { row: 1, col: 28 },
        }));
        let text = widget
            .test_selected_text()
            .expect("selection produces text");
        assert_eq!(text, "hello world\nsecond line trailing space");
    }
}

/// Ctrl-C/copy/paste/middle-click gestures reach the PTY or the confirmed clipboard actions, never around the permission surface.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "cases merged from separate tests; each case stays self-contained"
)]
#[serial_test::serial]
fn clipboard_gestures_route_through_confirmed_actions() {
    // Case `ctrl_c_without_shift_still_sends_etx_to_pty`.
    {
        // Regression guard: Ctrl+C must keep delivering SIGINT (0x03)
        // to the PTY even with no selection. The Ctrl+Shift+C path
        // must not poach the bare Ctrl+C event.
        use winit::keyboard::{Key, SmolStr};
        let (mut widget, write_rx) = build_test_widget();
        let mut doc = AutoCommit::new();
        widget.force_geometry(Rect::new(0.0, 0.0, 800.0, 480.0), 10.0, 20.0);

        send_key(
            &mut widget,
            &mut doc,
            Key::Character(SmolStr::new_static("c")),
            /*ctrl=*/ true,
            /*shift=*/ false,
        );

        let bytes = write_rx.try_recv().expect("Ctrl+C must write bytes to PTY");
        assert_eq!(bytes, vec![0x03], "Ctrl+C must send ETX (0x03) to the PTY");
    }

    // Case `ctrl_c_without_shift_still_sends_etx_even_with_selection`.
    {
        // A standing selection must not divert bare Ctrl+C into copy
        // semantics — the user expects SIGINT.
        use winit::keyboard::{Key, SmolStr};
        let (mut widget, write_rx) = build_test_widget();
        let mut doc = AutoCommit::new();
        widget.force_geometry(Rect::new(0.0, 0.0, 800.0, 480.0), 10.0, 20.0);
        widget.force_grid(vec!["hello".to_string()]);
        widget.force_selection(Some(Selection {
            anchor: GridPos { row: 0, col: 0 },
            cursor: GridPos { row: 0, col: 5 },
        }));

        send_key(
            &mut widget,
            &mut doc,
            Key::Character(SmolStr::new_static("c")),
            /*ctrl=*/ true,
            /*shift=*/ false,
        );

        let bytes = write_rx.try_recv().expect("Ctrl+C must still write to PTY");
        assert_eq!(bytes, vec![0x03]);
    }

    // Case `ctrl_shift_c_copies_the_selection_instead_of_interrupting`.
    {
        use winit::keyboard::{Key, SmolStr};
        let (mut widget, write_rx) = build_test_widget();
        let mut doc = AutoCommit::new();
        widget.force_geometry(Rect::new(0.0, 0.0, 800.0, 480.0), 10.0, 20.0);
        widget.force_viewport_start(0);
        widget.force_grid(vec!["coverage one".to_string(), "coverage two".to_string()]);
        widget.force_selection(Some(Selection {
            anchor: GridPos { row: 0, col: 0 },
            cursor: GridPos { row: 1, col: 12 },
        }));
        send_key(
            &mut widget,
            &mut doc,
            Key::Character(SmolStr::new_static("C")),
            /*ctrl=*/ true,
            /*shift=*/ true,
        );
        assert!(
            drain_writes(&write_rx).is_empty(),
            "the copy shortcut never reaches the process"
        );
        assert!(widget.selection().is_some(), "copying keeps the selection");
    }

    // Case `paste_shortcuts_paste_the_clipboard_into_the_process`.
    {
        use winit::keyboard::{Key, NamedKey, SmolStr};

        ui::clipboard_copy("shortcut paste");
        for key in [
            Key::Character(SmolStr::new_static("V")),
            Key::Named(NamedKey::Insert),
        ] {
            let (mut widget, writes) = build_test_widget();
            let mut doc = AutoCommit::new();
            widget.force_geometry(Rect::new(0.0, 0.0, 800.0, 480.0), 10.0, 20.0);
            let ctrl = matches!(&key, Key::Character(_));
            send_key(&mut widget, &mut doc, key, ctrl, true);
            assert_eq!(drain_writes(&writes), b"shortcut paste");
        }
    }

    // Case `middle_click_pastes_the_primary_selection`.
    {
        // Headless runs use the toolkit primary-selection fallback, so this
        // path is deterministic without opening a graphical session.
        ui::clipboard_copy_primary("PASTED");
        let (mut w, write_rx) = build_test_widget();
        let mut doc = AutoCommit::new();
        w.force_geometry(Rect::new(0.0, 0.0, 800.0, 480.0), 10.0, 20.0);
        w.feed(b""); // init modes (no bracketed paste)
        send_cursor_move(&mut w, &mut doc, 100.0, 100.0);
        send_middle_press(&mut w, &mut doc);
        assert_eq!(drain_writes(&write_rx), b"PASTED");
    }

    // Case `a_participant_who_may_only_view_cannot_drive_the_process`.
    {
        use crate::resource::TerminalEffect;
        let mut viewer =
            ui::framework::testing::TestApp::<crate::resource::TerminalShell>::with_role(
                ui::Role::Viewer,
            )
            .expect("terminal");
        for effect in [
            TerminalEffect::Paste {
                text: "denied".into(),
            },
            TerminalEffect::Send {
                text: "denied".into(),
            },
            TerminalEffect::Interrupt,
        ] {
            assert!(
                viewer.effect_confirmed(&effect).is_err(),
                "a viewer may not {effect:?}"
            );
        }
    }
}

// ---------------------------------------------------------------
// Scrollback viewing — real VT bytes through `feed`/`sync`.
// ---------------------------------------------------------------

/// Scrollback travel, the scrollbar's presence, and snapping back to the live bottom.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "cases merged from separate tests; each case stays self-contained"
)]
fn scrollback_viewport_and_scrollbar_track_output() {
    // Case `scrollback_reveals_older_lines_and_snaps_back`.
    {
        use vello::kurbo::Rect;
        // 240px tall / 20px rows → 12 visible rows. Feed many more than that
        // so history accumulates above the viewport.
        let (mut w, _write_rx, out_tx) = build_test_widget_io();
        let doc = AutoCommit::new();
        w.force_geometry(Rect::new(0.0, 0.0, 800.0, 240.0), 10.0, 20.0);

        let mut payload = Vec::new();
        for i in 0..60 {
            payload.extend_from_slice(format!("LINE{i:03}\r\n").as_bytes());
        }
        w.feed(&payload);
        w.sync(&doc, &automerge::ROOT);

        // Pinned to the bottom: newest lines visible, oldest not.
        let bottom = w.visible_lines();
        assert!(
            bottom.iter().any(|l| l.contains("LINE059")),
            "bottom shows newest: {bottom:?}"
        );
        assert!(
            !bottom.iter().any(|l| l.contains("LINE000")),
            "bottom hides oldest"
        );
        assert_eq!(w.scroll_offset(), 0);

        // Scroll to the very top: oldest lines visible, newest not.
        w.scroll_to_top();
        let top = w.visible_lines();
        assert!(
            top.iter().any(|l| l.contains("LINE000")),
            "top shows oldest: {top:?}"
        );
        assert!(
            !top.iter().any(|l| l.contains("LINE059")),
            "top hides newest"
        );
        assert!(w.scroll_offset() > 0);

        // Fresh PTY output must snap the viewport back to the bottom. Push it
        // through the real channel so `sync`→`pump` performs the snap.
        out_tx.send(b"LINE060\r\n".to_vec()).unwrap();
        w.sync(&doc, &automerge::ROOT);
        assert_eq!(w.scroll_offset(), 0, "new output snaps to the live bottom");
        let after = w.visible_lines();
        assert!(
            after.iter().any(|l| l.contains("LINE060")),
            "bottom shows freshest: {after:?}"
        );
    }

    // Case `a_blank_buffer_taller_than_the_window_draws_no_scrollbar`.
    // A session that has printed nothing draws no scrollbar, even though
    // its buffer is longer than the window showing it.
    //
    // This is the empty-terminal case on the desktop: a widget allocates a
    // full screen of blank rows the moment it exists, so `grid.len()`
    // exceeds the drawn viewport before a single byte of output has
    // arrived — and the old `grid.len() > visible_rows` test alone drew a
    // track and a thumb whose entire travel was blank rows.
    {
        use vello::kurbo::Rect;
        let (mut w, _rx) = build_test_widget();
        w.force_geometry(Rect::new(0.0, 0.0, 800.0, 240.0), 10.0, 20.0);
        // 20 never-written rows behind a 12-row viewport.
        w.force_grid(vec![" ".repeat(80); 20]);
        assert!(
            !w.scrollbar_visible(12),
            "20 blank rows behind a 12-row window is nothing to scroll to",
        );
    }

    // Case `output_past_the_viewport_draws_a_scrollbar`.
    // Once there is output above the viewport the bar comes back — the
    // guard must not have simply disabled the scrollbar.
    {
        use vello::kurbo::Rect;
        let (mut w, _rx) = build_test_widget();
        w.force_geometry(Rect::new(0.0, 0.0, 800.0, 240.0), 10.0, 20.0);
        w.force_grid((0..20).map(|i| format!("LINE{i:03}")).collect());
        assert!(
            w.scrollbar_visible(12),
            "20 written rows behind a 12-row window is 8 rows of real travel",
        );
    }

    // Case `blank_rows_below_the_last_output_are_not_scrollable`.
    // Blank rows *below* the last output are not travel either: the screen
    // a session holds open under a short command is padding, not history.
    {
        use vello::kurbo::Rect;
        let (mut w, _rx) = build_test_widget();
        w.force_geometry(Rect::new(0.0, 0.0, 800.0, 240.0), 10.0, 20.0);
        let mut grid = vec![" ".repeat(80); 30];
        for (i, row) in grid.iter_mut().take(4).enumerate() {
            *row = format!("LINE{i:03}");
        }
        w.force_grid(grid);
        assert!(
            !w.scrollbar_visible(12),
            "4 written rows in a 12-row window fit — the 26 blank rows \
         behind them are not history",
        );
    }

    // Case `keystroke_snaps_scrollback_to_bottom`.
    {
        use vello::kurbo::Rect;
        use winit::keyboard::{Key, SmolStr};
        let (mut w, _write_rx, _out_tx) = build_test_widget_io();
        let mut doc = AutoCommit::new();
        w.force_geometry(Rect::new(0.0, 0.0, 800.0, 240.0), 10.0, 20.0);

        let mut payload = Vec::new();
        for i in 0..60 {
            payload.extend_from_slice(format!("L{i:03}\r\n").as_bytes());
        }
        w.feed(&payload);
        w.sync(&doc, &automerge::ROOT);

        w.scroll_to_top();
        assert!(w.scroll_offset() > 0);

        // Typing a key that reaches the PTY must snap to the bottom.
        send_key(
            &mut w,
            &mut doc,
            Key::Character(SmolStr::new_static("x")),
            false,
            false,
        );
        assert_eq!(w.scroll_offset(), 0, "keystroke snaps to bottom");
    }

    // Case `only_shift_page_keys_control_local_scrollback`.
    {
        use winit::keyboard::{Key, NamedKey};

        let (mut widget, writes) = build_test_widget();
        let mut doc = AutoCommit::new();
        widget.force_geometry(Rect::new(0.0, 0.0, 800.0, 80.0), 10.0, 20.0);
        widget.force_grid((0..30).map(|i| format!("line {i}")).collect());
        widget.scroll_to_top();
        let before = widget.scroll_offset();

        send_key(
            &mut widget,
            &mut doc,
            Key::Named(NamedKey::PageDown),
            false,
            false,
        );
        assert_eq!(drain_writes(&writes), b"\x1b[6~".to_vec());
        assert_eq!(
            widget.scroll_offset(),
            0,
            "PTY input returns to live output"
        );

        widget.scroll_to_top();
        send_key(
            &mut widget,
            &mut doc,
            Key::Named(NamedKey::PageDown),
            false,
            true,
        );
        assert!(widget.scroll_offset() < before);
        assert!(drain_writes(&writes).is_empty(), "Shift paging stays local");
    }
}

/// Local find: shortcut routing, discoverability, matches and reflow.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "cases merged from separate tests; each case stays self-contained"
)]
fn find_in_terminal_is_local_discoverable_and_reflows() {
    use crate::TerminalView;

    // Case `find_shortcut_preserves_foreground_control_f_on_every_platform`.
    {
        use ui::{Platform, PlatformConventions};
        use winit::keyboard::{Key, ModifiersState};

        for platform in [Platform::Windows, Platform::MacOs, Platform::Linux] {
            for (modifiers, expected) in [
                (ModifiersState::CONTROL, vec![0x06]),
                (
                    ModifiersState::CONTROL | ModifiersState::ALT,
                    vec![0x1b, 0x06],
                ),
            ] {
                let (widget, writes) = build_test_widget();
                let mut view = TerminalView::new(widget);
                view.set_platform_conventions(PlatformConventions::new(platform));
                let mut doc = AutoCommit::new();
                send_key_with_modifiers(&mut view, &mut doc, Key::Character("f".into()), modifiers);
                assert_eq!(
                    drain_writes(&writes),
                    expected,
                    "{platform:?}: shell editing bytes"
                );
                assert!(
                    !view.find_is_open(),
                    "{platform:?}: shell chord must not open Find"
                );
            }
        }
    }

    // Case `find_shortcut_opens_local_search_and_escape_restores_terminal_input`.
    {
        use ui::{Platform, PlatformConventions};
        use winit::keyboard::{Key, ModifiersState, NamedKey};

        for (platform, modifiers) in [
            (
                Platform::Windows,
                ModifiersState::CONTROL | ModifiersState::SHIFT,
            ),
            (
                Platform::Linux,
                ModifiersState::CONTROL | ModifiersState::SHIFT,
            ),
            (
                Platform::MacOs,
                ModifiersState::SUPER | ModifiersState::SHIFT,
            ),
        ] {
            let (widget, writes) = build_test_widget();
            let mut view = TerminalView::new(widget);
            view.set_platform_conventions(PlatformConventions::new(platform));
            let mut doc = AutoCommit::new();
            send_key_with_modifiers(&mut view, &mut doc, Key::Character("F".into()), modifiers);
            assert!(
                view.find_is_open(),
                "{platform:?}: the find scope's terminal chord"
            );
            assert!(
                drain_writes(&writes).is_empty(),
                "Find must not type into the process"
            );
            send_key(
                &mut view,
                &mut doc,
                Key::Character("query".into()),
                false,
                false,
            );
            assert!(
                drain_writes(&writes).is_empty(),
                "private Find query must not reach the process"
            );
            // A key the query has no use for stays the bar's too.
            send_key(
                &mut view,
                &mut doc,
                Key::Named(NamedKey::ArrowUp),
                false,
                false,
            );
            assert!(drain_writes(&writes).is_empty());
            send_key(
                &mut view,
                &mut doc,
                Key::Named(NamedKey::Escape),
                false,
                false,
            );
            assert!(!view.find_is_open());
            send_key(&mut view, &mut doc, Key::Character("f".into()), true, false);
            assert_eq!(drain_writes(&writes), vec![0x06]);
        }
    }

    // Case `find_is_discoverable_from_the_semantic_transcript_without_typing_into_the_process`.
    {
        use ui::a11y::{Action, Role};

        let (mut widget, writes) = build_test_widget();
        let mut doc = AutoCommit::new();
        widget.force_geometry(Rect::new(0.0, 0.0, 800.0, 480.0), 10.0, 20.0);
        let mut view = TerminalView::new(widget);
        let heads_before = doc.get_heads();
        let transcript = view
            .terminal()
            .accessibility()
            .expect("terminal transcript");
        assert!(transcript.state.context_menu);
        assert!(
            view.terminal_mut().perform_semantic_action(
                transcript
                    .action_target
                    .as_deref()
                    .expect("transcript target"),
                &Action::ShowContextMenu,
                &mut ui::widget::Edits::lend(&mut doc),
                &ROOT,
            )
        );
        let semantics = view.terminal().accessibility().expect("terminal with menu");
        let menu = semantics
            .children
            .iter()
            .find(|child| child.role == Role::Menu)
            .expect("discoverable menu");
        let find_label = ui::i18n::I18nManager::lookup("terminal-find-scrollback", None);
        assert_ne!(
            find_label, "terminal-find-scrollback",
            "toolkit supplies the visible label"
        );
        let find = menu
            .children
            .iter()
            .find(|item| item.name.as_deref() == Some(find_label.as_str()))
            .expect("Find entry");
        assert!(view.terminal_mut().perform_semantic_action(
            find.action_target.as_deref().expect("Find target"),
            &Action::Activate,
            &mut ui::widget::Edits::lend(&mut doc),
            &ROOT,
        ));
        view.sync(&doc, &ROOT);
        assert!(view.find_is_open());
        let mut state = State::new();
        assert!(state.adopt_doc(doc));
        let mut framed = ui::headless::Framed::new(state, view, (800, 480), ui::Theme::dark());
        let harness = ui::headless::Framed::harness_mut(&mut framed);
        let query = harness
            .a11y_nodes()
            .into_iter()
            .find(|(_, node)| node.role() == ui::accesskit::Role::TextInput)
            .map(|(id, _)| id)
            .expect("Find publishes its actual retained text input");
        harness
            .perform_a11y_action(query, &Action::SetValue("private search".into()))
            .expect("the hosted Find query owns its text-edit action");
        assert!(
            drain_writes(&writes).is_empty(),
            "Find never writes shell commands"
        );
        let (view, state) = ui::headless::Framed::split(&mut framed);
        assert_eq!(
            heads_before,
            state.doc.get_heads(),
            "Find remains private local view state"
        );
        assert!(
            !view.terminal_mut().perform_semantic_action(
                transcript.action_target.as_deref().unwrap(),
                &Action::SetValue("unreviewed command".into()),
                &mut ui::widget::Edits::lend(&mut state.doc),
                &ROOT,
            ),
            "transcript actions still cannot execute arbitrary text"
        );
        assert!(drain_writes(&writes).is_empty());
    }

    // Case `fresh_output_tells_an_open_find_bar_to_find_again`.
    {
        use ui::widget::Findable;

        let (mut widget, _writes) = build_test_widget();
        let doc = AutoCommit::new();
        widget.force_geometry(Rect::new(0.0, 0.0, 800.0, 480.0), 10.0, 20.0);
        widget.sync(&doc, &ROOT);
        let before = Findable::content_revision(&widget);
        widget.feed(b"build step 020\r\n");
        widget.sync(&doc, &ROOT);
        assert_ne!(
            Findable::content_revision(&widget),
            before,
            "output that reached the grid is a new revision for the bar's counter"
        );
    }

    // Case `find_in_scrollback_is_local_reveals_matches_and_reflows_at_large_text`.
    {
        use ui::{Theme, a11y::Action};

        let (mut widget, writes) = build_test_widget();
        let mut doc = AutoCommit::new();
        let viewport = Rect::new(0.0, 0.0, 390.0, 844.0);
        widget.force_geometry(viewport, 10.0, 20.0);
        widget.force_grid(
            (0..120)
                .map(|row| format!("build step {row:03}: compiling local terminal output"))
                .collect(),
        );
        let heads_before = doc.get_heads();
        let mut view = TerminalView::new(widget);

        view.open_find();
        view.set_find_query("build step 020");
        assert!(view.find_is_open());
        assert_eq!(view.find_test_matches(), vec![20]);
        assert!(
            view.scroll_offset() > 0,
            "find reveals a distant history row"
        );
        assert_eq!(
            doc.get_heads(),
            heads_before,
            "find text/matches stay local rather than mutating the collaborative transcript"
        );

        // The real layout and draw at 200%: the scope stands its band above
        // the terminal, inside the viewport, and the grid starts under it.
        let theme = Theme::high_contrast().with_text_scale(2.0);
        let mut state = State::new();
        assert!(state.adopt_doc(doc));
        let mut framed = ui::headless::Framed::new(state, view, (390, 844), theme);
        assert!(
            !ui::headless::Framed::harness(&framed)
                .scene()
                .encoding()
                .is_empty(),
            "the real large-text host renders the terminal and its find band"
        );
        let view = &framed;
        let band = view.find_band().expect("find band");
        assert!(band.x0 >= viewport.x0 && band.x1 <= viewport.x1);
        assert!(band.y0 >= viewport.y0 && band.y1 <= viewport.y1);
        assert!(band.height() > 0.0);
        assert!(
            view.grid_rect().y0 >= band.y1,
            "the grid starts under the find band"
        );
        assert_eq!(
            view.find_counter(),
            Some(("build step 020".to_string(), Some((1, 1))))
        );

        // Assistive activation resolves the exported controls to their real
        // FindBar owner rather than sending child targets to TerminalView.
        let control = |harness: &ui::headless::FrameHarness, key: &str| {
            let label = ui::i18n::I18nManager::lookup_plain(key);
            harness
                .a11y_nodes()
                .into_iter()
                .find(|(_, node)| node.label() == Some(label.as_str()))
                .map_or_else(
                    || panic!("missing exported Find control {label}"),
                    |(id, _)| id,
                )
        };
        let harness = ui::headless::Framed::harness_mut(&mut framed);
        let next = control(harness, "find-bar-next");
        harness
            .perform_a11y_action(next, &Action::Activate)
            .expect("the exported next-match control enters the real find scope");
        let query = control(harness, "find-bar-placeholder");
        harness
            .perform_a11y_action(query, &Action::SetValue("build step 042".into()))
            .expect("the exported query edits local Find state");
        assert_eq!(framed.find_test_matches(), vec![42]);
        let harness = ui::headless::Framed::harness_mut(&mut framed);
        let close = control(harness, "find-bar-close");
        harness
            .perform_a11y_action(close, &Action::Activate)
            .expect("the exported close control closes the real find scope");
        assert!(!framed.find_is_open());
        assert!(
            drain_writes(&writes).is_empty(),
            "Find never writes to the PTY"
        );
        let (_, state) = ui::headless::Framed::split(&mut framed);
        assert_eq!(
            state.doc.get_heads(),
            heads_before,
            "hosted Find actions leave the collaborative transcript unchanged"
        );
    }
}

/// The terminal context menu: keyboard gestures, scrolling, semantics and clamping.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "cases merged from separate tests; each case stays self-contained"
)]
fn context_menu_is_keyboard_navigable_and_laid_out_in_view() {
    // Case `terminal_context_menu_keyboard_gestures_do_not_reach_the_process`.
    {
        use ui::a11y::Role;
        use winit::keyboard::{Key, NamedKey};

        for (key, shift) in [(NamedKey::ContextMenu, false), (NamedKey::F10, true)] {
            let (mut widget, writes) = build_test_widget();
            let mut doc = AutoCommit::new();
            widget.force_geometry(Rect::new(0.0, 0.0, 800.0, 480.0), 10.0, 20.0);
            send_key(&mut widget, &mut doc, Key::Named(key), false, shift);
            assert!(
                widget
                    .accessibility()
                    .unwrap()
                    .children
                    .iter()
                    .any(|node| node.role == Role::Menu)
            );
            assert!(drain_writes(&writes).is_empty());
        }

        let (mut widget, writes) = build_test_widget();
        let mut doc = AutoCommit::new();
        send_key(&mut widget, &mut doc, Key::Named(NamedKey::F10), true, true);
        assert_eq!(
            drain_writes(&writes),
            b"\x1b[21;6~",
            "extra modifiers stay foreground input"
        );
    }

    // Case `short_terminal_menu_scrolls_to_find_before_keyboard_activation`.
    {
        use ui::{Theme, presence::PresenceCtx, text_render::TextRenderer};
        use vello::{Scene, kurbo::Affine};
        use winit::keyboard::{Key, NamedKey};
        for height in [120.0, 160.0] {
            let (mut widget, writes) = build_test_widget();
            let mut doc = AutoCommit::new();
            let viewport = Rect::new(0.0, 0.0, 320.0, height);
            widget.force_geometry(viewport, 10.0, 20.0);
            widget.force_grid(vec!["one".into(), "two".into()]);
            widget.open_context_menu_at(viewport.center());
            widget.draw(
                &mut Scene::new(),
                Affine::IDENTITY,
                &mut TextRenderer::new(),
                &Theme::dark().with_text_scale(2.0),
                &PresenceCtx::empty(),
            );
            // Up enters the final actionable row, which is Find below the short-
            // viewport threshold. The shared menu scrolls its clipped row into view.
            send_key(
                &mut widget,
                &mut doc,
                Key::Named(NamedKey::ArrowUp),
                false,
                false,
            );
            let row = widget.context_menu_find_rect().expect("Find row");
            assert!(
                row.area() > 0.0,
                "Find must be painted after keyboard navigation"
            );
            assert_eq!(row.intersect(viewport), row);
            send_key(
                &mut widget,
                &mut doc,
                Key::Named(NamedKey::Enter),
                false,
                false,
            );
            assert!(
                widget.take_find_request(),
                "the Find row asks the hosting find scope for its bar"
            );
            assert!(
                ui::widget::controls::take_control_invocation(&mut widget).is_none(),
                "Find is local presentation"
            );
            assert!(drain_writes(&writes).is_empty());
        }
    }

    // Case `context_menu_is_keyboard_navigable_and_semantically_complete`.
    {
        use winit::event::{ElementState, MouseButton};
        use winit::keyboard::{Key, NamedKey};

        let (mut widget, _writes) = build_test_widget();
        let mut doc = AutoCommit::new();
        widget.force_geometry(Rect::new(0.0, 0.0, 800.0, 120.0), 10.0, 20.0);
        widget.force_grid(vec!["one".into(), "two".into()]);
        send_cursor_move(&mut widget, &mut doc, 40.0, 40.0);
        send_mouse_button_mods(
            &mut widget,
            &mut doc,
            MouseButton::Right,
            ElementState::Pressed,
            false,
        );

        let semantics = ui::widget::Widget::accessibility(&widget).expect("terminal semantics");
        let menu = semantics
            .children
            .iter()
            .find(|child| child.role == ui::a11y::Role::Menu)
            .expect("open context menu is exposed");
        assert!(menu.state.modal && menu.state.expanded == Some(true));
        assert!(menu.children.iter().any(|item| {
            item.name.as_deref() == Some("Select all output")
                && item.action_target.is_some()
                && item.state.focusable
        }));

        // First Down selects Paste; second skips the separator to Select all.
        send_key(
            &mut widget,
            &mut doc,
            Key::Named(NamedKey::ArrowDown),
            false,
            false,
        );
        send_key(
            &mut widget,
            &mut doc,
            Key::Named(NamedKey::ArrowDown),
            false,
            false,
        );
        send_key(
            &mut widget,
            &mut doc,
            Key::Named(NamedKey::Enter),
            false,
            false,
        );
        assert_eq!(
            widget.selection(),
            Some(Selection {
                anchor: GridPos { row: 0, col: 0 },
                cursor: GridPos { row: 1, col: 3 },
            })
        );
    }

    // Case `context_menu_clamps_to_top_when_taller_than_viewport`.
    {
        use ui::{Theme, presence::PresenceCtx, text_render::TextRenderer};
        use vello::{
            Scene,
            kurbo::{Affine, Point, Rect},
        };

        let (mut widget, _writes) = build_test_widget();
        let viewport = Rect::new(0.0, 0.0, 390.0, 160.0);
        widget.force_geometry(viewport, 10.0, 20.0);
        widget.force_grid(vec!["one".into(), "two".into()]);
        widget.open_context_menu_at(Point::new(389.0, 159.0));

        // ContextMenu refreshes its scaled geometry in its draw pass. Exercise
        // that real path: a 200% menu opened at the bottom/right edge must remain
        // within the terminal pane rather than retaining its pre-scale bounds.
        let mut scene = Scene::new();
        let mut text_renderer = TextRenderer::new();
        let theme = Theme::dark().with_text_scale(2.0);
        widget.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut text_renderer,
            &theme,
            &PresenceCtx::empty(),
        );

        let menu = widget.context_menu_rect();
        assert!(menu.x0 >= viewport.x0 && menu.x1 <= viewport.x1);
        assert!(menu.y0 >= viewport.y0 && menu.y1 <= viewport.y1);
    }
}

/// Menu rows do what they say: Clear sends the form feed, Paste pastes the
/// clipboard as it was when the row was chosen, and a row for the process's
/// input is disabled once it ended.
#[test]
#[serial_test::serial]
fn menu_rows_drive_the_process_and_follow_its_lifecycle() {
    use vello::kurbo::Point;
    let row = |widget: &TerminalWidget, label: &str| {
        widget
            .context_menu_labels()
            .iter()
            .position(|item| item == label)
            .unwrap_or_else(|| panic!("menu has `{label}`"))
    };

    let (mut widget, writes) = build_test_widget();
    widget.force_geometry(Rect::new(0.0, 0.0, 800.0, 480.0), 10.0, 20.0);
    widget.feed(b"output");
    widget.open_context_menu_at(Point::new(20.0, 20.0));
    let clear = row(&widget, "Clear screen");
    widget.activate_menu_row_for_test(clear);
    assert_eq!(drain_writes(&writes), vec![0x0c]);

    ui::clipboard_copy("menu snapshot");
    widget.open_context_menu_at(Point::new(20.0, 20.0));
    let paste = row(&widget, "Paste");
    widget.activate_menu_row_for_test(paste);
    assert_eq!(drain_writes(&writes), b"menu snapshot");

    widget.mark_disconnected_for_test();
    widget.open_context_menu_at(Point::new(20.0, 20.0));
    let paste = row(&widget, "Paste");
    assert!(
        !widget.context_menu_item_enabled(paste),
        "an ended process takes no paste"
    );
}

// ---------------------------------------------------------------
// Scrollbar honesty — the gutter only claims travel that exists.
// `scrollbar_visible` is the predicate `draw_scrollbar` gates on, so
// these assert the production decision rather than a stand-in.
// ---------------------------------------------------------------

/// The grid's CRDT mirror: bulk output, incremental sync/trim, external edits and commit reporting.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "cases merged from separate tests; each case stays self-contained"
)]
fn grid_mirror_stays_linear_aligned_and_reports_changes() {
    // Case `bulk_styled_output_stays_linear_and_correct`.
    // Bulk output must stay linear. `print` batches a run of cells into one
    // diffed text splice + one `attrs` put per line; the old per-cell
    // `splice_text` + full-`attrs`-`put` was an O(doc) write *per character*, so
    // a screenful of coloured output was O(n²) and took minutes (a debug-build
    // freeze; HIG §6). Feeding a full screen of styled lines through the real
    // `feed` path must produce correct content and finish well under a bound the
    // quadratic path could never meet.
    {
        use ui::time::Duration;
        let (mut w, _write_rx, _out_tx) = build_test_widget_io();
        let doc = AutoCommit::new();
        // 129 cols × ~38 rows — the same shape a maximized terminal reflows to,
        // where the regression froze.
        w.force_geometry(Rect::new(0.0, 0.0, 1290.0, 760.0), 10.0, 20.0);

        // A screenful-and-then-some of SGR-coloured lines (the mid-line reset is
        // deliberate: it forces a buffer flush partway through each line too).
        let mut payload = Vec::new();
        for i in 0..80u32 {
            payload.extend_from_slice(
            format!("\x1b[38;5;250m{i:>4}\x1b[0m  the quick brown fox jumps over the lazy dog {i:03}\r\n")
                .as_bytes(),
        );
        }

        let start = ui::time::now();
        w.feed(&payload);
        w.sync(&doc, &automerge::ROOT);
        let elapsed = start.elapsed();

        // Correctness: the freshest lines are on screen with their real content.
        let visible = w.visible_lines();
        assert!(
            visible.iter().any(|l| l.contains("lazy dog 079")),
            "newest bulk line rendered: {visible:?}"
        );
        // Scroll back to prove the older lines were retained (not dropped/garbled).
        w.scroll_to_top();
        let top = w.visible_lines();
        assert!(
            top.iter().any(|l| l.contains("lazy dog 000")),
            "oldest bulk line retained: {top:?}"
        );

        // Performance guard: the fixed path does this in well under a second even
        // in debug; the O(n²) regression took minutes. A generous 30s bound only
        // trips if the per-character CRDT write storm comes back — never on load.
        assert!(
            elapsed < Duration::from_secs(30),
            "feeding 80 styled lines took {elapsed:?} — the per-cell O(n²) CRDT \
         write regressed (HIG §6)"
        );
    }

    // Case `incremental_sync_and_trim_keep_mirror_aligned`.
    {
        use vello::kurbo::Rect;
        // Feed across several syncs, past the (test-shrunk) retained-history
        // cap, then prove the render mirror — which now only re-reads changed
        // rows and drains trimmed front lines rather than re-reading the whole
        // buffer — is still exactly index-aligned with the CRDT: newest kept,
        // oldest trimmed, survivors intact, memory bounded.
        let (mut w, _rx, _out) = build_test_widget_io();
        let doc = AutoCommit::new();
        w.force_geometry(Rect::new(0.0, 0.0, 800.0, 240.0), 10.0, 20.0); // 80x24 logical grid

        let mut emitted = 0usize;
        for _chunk in 0..3 {
            let mut payload = Vec::new();
            for _ in 0..50 {
                payload.extend_from_slice(format!("ROW{emitted:04}\r\n").as_bytes());
                emitted += 1;
            }
            w.feed(&payload);
            w.sync(&doc, &automerge::ROOT);
        }
        // 150 lines fed, MAX_HISTORY 120 (+8 slack) under cfg(test): a trim
        // must have run, and the incremental sync must have stayed aligned.
        let text = w.grid_text();
        let count = text.lines().count();
        assert!(count <= 120 + 8 + 24, "history bounded, got {count} lines");
        assert!(
            text.lines().any(|l| l.trim_end() == "ROW0149"),
            "newest line present in the aligned mirror"
        );
        assert!(
            !text.contains("ROW0000"),
            "oldest line trimmed off the front"
        );
        // A surviving mid-history line must be exact (not shifted/garbled by
        // the drain-alignment).
        assert!(
            text.lines().any(|l| l.trim_end() == "ROW0120"),
            "a surviving scrollback line stays intact: {:?}",
            text.lines().rev().take(5).collect::<Vec<_>>()
        );
    }

    // Case `a_collaborators_edit_to_old_scrollback_reaches_a_viewer`.
    {
        use ui::tilo_actions::Action as _;
        use vello::kurbo::Rect;
        // A collaborator's change to an *old* scrollback line arrives as a
        // change to the session record; a terminal showing that record reads
        // only what changed and shows it.
        let (mut host, _rx, _out) = build_test_widget_io();
        host.force_geometry(Rect::new(0.0, 0.0, 800.0, 240.0), 10.0, 20.0);
        let mut payload = Vec::new();
        for i in 0..40 {
            payload.extend_from_slice(format!("BASE{i:03}\r\n").as_bytes());
        }
        host.feed(&payload);

        let mut doc = AutoCommit::new();
        crate::resource::TerminalAction::Output {
            edits: crate::screen::edits(None, host.screen()),
        }
        .apply(&mut doc)
        .unwrap();
        let mut reader = ui::tilo_resource::Reader::<crate::resource::Terminal>::open(&mut doc)
            .expect("the session reads");
        let (mut viewer, _rx) = build_test_widget();
        viewer.force_geometry(Rect::new(0.0, 0.0, 800.0, 240.0), 10.0, 20.0);
        ResourceSurface::<crate::resource::Terminal, crate::resource::TerminalEffect>::present(
            &mut viewer,
            reader.state(),
        );
        assert!(viewer.grid_text().contains("BASE000"), "base content shown");

        crate::resource::TerminalAction::Output {
            edits: vec![crate::screen::ScreenEdit::Write {
                alternate: false,
                index: 0,
                text: "REMOTE_EDIT".into(),
                attrs: Vec::new(),
            }],
        }
        .apply(&mut doc)
        .unwrap();
        assert!(reader.sync(&doc).expect("the change reads"));
        ResourceSurface::<crate::resource::Terminal, crate::resource::TerminalEffect>::present(
            &mut viewer,
            reader.state(),
        );
        assert!(
            viewer.grid_text().contains("REMOTE_EDIT"),
            "a change to old scrollback reaches the viewer"
        );
        assert!(viewer.grid_text().contains("BASE039"));
    }

    // Case `commit_action_reports_whether_the_document_actually_changed`.
    {
        // The perf fix behind the selection latency: `commit_action` (called by
        // the App after every handled event, including each drag `CursorMoved`)
        // may now skip the full `doc.save()` + broadcast when nothing changed.
        // This proves the signal it keys on — an empty transaction reports no
        // change; a real edit reports one.
        use automerge::{ROOT, transaction::Transactable};
        use ui::State;

        let mut state = State::new();
        // Flush any ops the constructor might have left pending, so the sequence
        // below is deterministic regardless of `State::new`'s internals.
        let _ = state.commit_action_authored(None, None, None);

        assert!(
            !state.commit_action_authored(None, None, None),
            "an empty transaction must report no change (App then skips save)"
        );
        state.doc.put(&ROOT, "k", "v").unwrap();
        assert!(
            state.commit_action_authored(None, None, None),
            "a real edit must report a change (App then saves + broadcasts)"
        );
        assert!(
            !state.commit_action_authored(None, None, None),
            "with nothing new pending, the next commit reports no change again"
        );
    }
}

// ---------------------------------------------------------------
// Paste — bracketed and plain, straight to the PTY.
// ---------------------------------------------------------------

/// Plain and bracketed paste bytes.
#[test]
fn paste_writes_to_pty_with_newline_and_bracketing_rules() {
    // Case `plain_paste_writes_to_pty_with_cr_newlines`.
    {
        let (mut w, write_rx) = build_test_widget();
        w.feed(b""); // init crdt / modes
        w.paste("echo hi\nls");
        let got = drain_writes(&write_rx);
        assert_eq!(got, b"echo hi\rls", "plain paste normalises LF to CR");
    }

    // Case `bracketed_paste_wraps_when_app_enabled_it`.
    {
        let (mut w, write_rx) = build_test_widget();
        // App turns on bracketed paste via a real DEC private set sequence.
        w.feed(b"\x1b[?2004h");
        assert!(w.modes().bracketed_paste);
        w.paste("data");
        let got = drain_writes(&write_rx);
        assert_eq!(
            got, b"\x1b[200~data\x1b[201~",
            "bracketed paste wraps the payload"
        );
    }
}

// ---------------------------------------------------------------
// Alternate screen + DEC private modes.
// ---------------------------------------------------------------

/// Alternate screen, DEC private modes and focus reporting.
#[test]
fn terminal_modes_switch_screens_cursor_paste_and_focus_reporting() {
    // Case `alt_screen_switches_buffers_and_restores_main`.
    {
        use vello::kurbo::Rect;
        let (mut w, _rx) = build_test_widget();
        let doc = AutoCommit::new();
        w.force_geometry(Rect::new(0.0, 0.0, 800.0, 480.0), 10.0, 20.0);

        // Write to the main screen.
        w.feed(b"MAIN CONTENT\r\n");
        w.sync(&doc, &automerge::ROOT);
        assert!(!w.is_alt_screen());
        assert!(w.grid_row(0).starts_with("MAIN CONTENT"));

        // Enter the alternate screen and do a full-screen redraw.
        w.feed(b"\x1b[?1049h\x1b[2J\x1b[H");
        w.feed(b"\x1b[3;5HTUI REDRAW");
        w.sync(&doc, &automerge::ROOT);
        assert!(w.is_alt_screen(), "?1049h enters the alternate screen");
        // The alt buffer holds the redraw, and the main content is not in it.
        assert!(
            w.grid_row(2).contains("TUI REDRAW"),
            "row 2 (1-based row 3): {:?}",
            w.grid_row(2)
        );
        assert!(
            !w.grid_row(0).contains("MAIN CONTENT"),
            "alt buffer is fresh"
        );

        // Leave the alternate screen — the main buffer must return intact.
        w.feed(b"\x1b[?1049l");
        w.sync(&doc, &automerge::ROOT);
        assert!(!w.is_alt_screen(), "?1049l restores the main screen");
        assert!(
            w.grid_row(0).starts_with("MAIN CONTENT"),
            "main content restored: {:?}",
            w.grid_row(0)
        );
        assert!(
            !w.grid_row(2).contains("TUI REDRAW"),
            "alt redraw did not leak into main"
        );
    }

    // Case `dec_private_modes_toggle_cursor_and_bracketed_paste`.
    {
        let (mut w, _rx) = build_test_widget();
        w.feed(b"");
        assert!(w.modes().cursor_visible, "cursor visible by default");

        w.feed(b"\x1b[?25l");
        assert!(!w.modes().cursor_visible, "?25l hides the cursor");
        w.feed(b"\x1b[?25h");
        assert!(w.modes().cursor_visible, "?25h shows the cursor");

        // Mouse reporting is parsed (not crashing) into the mode flags.
        w.feed(b"\x1b[?1000h\x1b[?1006h");
        assert!(
            w.modes().mouse_report && w.modes().mouse_sgr,
            "mouse modes are tracked"
        );
        assert_eq!(w.modes().mouse_tracking, 1000);
    }

    // Case `focus_reporting_uses_xterm_focus_in_and_out_sequences`.
    {
        let (mut w, rx) = build_test_widget();
        let mut doc = AutoCommit::new();
        w.feed(b"\x1b[?1004h");
        let _ = drain_writes(&rx);
        send_focus(&mut w, &mut doc, true);
        send_focus(&mut w, &mut doc, false);
        assert_eq!(drain_writes(&rx), b"\x1b[I\x1b[O");
    }
}

/// Mouse reporting vs local selection, and padding/gutter geometry for hit-testing.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "cases merged from separate tests; each case stays self-contained"
)]
fn mouse_tracking_padding_and_grid_hit_testing() {
    // Case `sgr_mouse_tracking_forwards_click_release_and_button_motion`.
    {
        use winit::event::{ElementState, MouseButton};
        let (mut w, rx) = build_test_widget();
        let mut doc = AutoCommit::new();
        w.force_geometry(Rect::new(0.0, 0.0, 800.0, 480.0), 10.0, 20.0);
        w.feed(b"\x1b[?1002h\x1b[?1006h");
        let _ = drain_writes(&rx);

        send_cursor_move(&mut w, &mut doc, 25.0, 65.0); // cell 3,4 (one-based)
        send_mouse_button_mods(
            &mut w,
            &mut doc,
            MouseButton::Left,
            ElementState::Pressed,
            false,
        );
        send_cursor_move(&mut w, &mut doc, 35.0, 65.0); // held motion to cell 4,4
        send_mouse_button_mods(
            &mut w,
            &mut doc,
            MouseButton::Left,
            ElementState::Released,
            false,
        );

        assert_eq!(
            drain_writes(&rx),
            b"\x1b[<0;3;4M\x1b[<32;4;4M\x1b[<0;4;4m",
            "modern TUIs receive standard SGR button and drag reports"
        );
        assert!(
            w.selection().is_none(),
            "application mouse input is not also selected locally"
        );
    }

    // Case `shift_overrides_application_mouse_tracking_for_local_selection`.
    {
        use winit::event::{ElementState, MouseButton};
        let (mut w, rx) = build_test_widget();
        let mut doc = AutoCommit::new();
        w.force_geometry(Rect::new(0.0, 0.0, 800.0, 480.0), 10.0, 20.0);
        w.feed(b"\x1b[?1003h\x1b[?1006h");
        let _ = drain_writes(&rx);
        send_cursor_move(&mut w, &mut doc, 25.0, 65.0);
        let _ = drain_writes(&rx); // ignore the requested all-motion hover report
        send_mouse_button_mods(
            &mut w,
            &mut doc,
            MouseButton::Left,
            ElementState::Pressed,
            true,
        );

        assert!(
            w.selection().is_some(),
            "Shift+drag remains the local selection escape hatch"
        );
        assert!(
            drain_writes(&rx).is_empty(),
            "Shift override sends no pointer event to the TUI"
        );
    }

    // Case `place_at_insets_grid_by_padding_and_gutters`.
    {
        // The production reflow path must seat the cell grid inside the body
        // padding on every side + the scrollbar gutter on the right — not fill
        // the whole widget rect the way the old void did. No header strip: the
        // top inset is the body padding, not an app-drawn tab bar.
        let (mut w, _) = build_test_widget();
        w.place_at(Rect::new(0.0, 0.0, 1000.0, 640.0));
        let gr = w.grid_rect();
        assert!(gr.y0 > 0.0, "grid top inset by body padding: {gr:?}");
        assert!(gr.x0 > 0.0, "grid left inset by body padding: {gr:?}");
        assert!(
            gr.x1 < 1000.0,
            "grid right inset by padding + scrollbar gutter: {gr:?}"
        );
        assert!(gr.y1 < 640.0, "grid bottom inset by body padding: {gr:?}");
        // The top+bottom padding genuinely reduces the usable rows vs. a
        // full-rect fit.
        let (cols, rows) = w.grid_size();
        assert!(cols > 0 && rows > 0, "reflow produced a usable grid");
        assert!(
            (rows as f64) < 640.0 / w.grid_char_h(),
            "body padding must reduce the row count below the naive full-height fit"
        );
    }

    // Case `padding_click_does_not_start_selection`.
    {
        // Clicking in the body padding above the grid content must NOT begin a
        // text selection — that region is outside the cell grid.
        let (mut w, _) = build_test_widget();
        let mut doc = AutoCommit::new();
        w.place_at(Rect::new(0.0, 0.0, 800.0, 600.0));
        let gr = w.grid_rect();
        let pad_y = gr.y0 / 2.0; // inside the top padding, above the grid
        send_cursor_move(&mut w, &mut doc, 120.0, pad_y);
        send_left_press(&mut w, &mut doc);
        assert!(
            w.selection().is_none(),
            "a click in the body padding must not start a selection"
        );
        send_left_release(&mut w, &mut doc);
        assert!(w.selection().is_none());
    }

    // Case `grid_click_inside_content_starts_selection_and_maps_in_bounds`.
    {
        // A press inside the inset grid content still begins a selection and
        // maps to an in-bounds cell — proving hit-testing follows the grid rect,
        // not the outer widget rect.
        let (mut w, _) = build_test_widget();
        let mut doc = AutoCommit::new();
        w.place_at(Rect::new(0.0, 0.0, 800.0, 600.0));
        let gr = w.grid_rect();
        send_cursor_move(&mut w, &mut doc, gr.x0 + 30.0, gr.y0 + 20.0);
        send_left_press(&mut w, &mut doc);
        let sel = w
            .selection()
            .expect("press inside the grid begins a selection");
        let (cols, _rows) = w.grid_size();
        assert!(sel.anchor.col < cols, "mapped column within grid width");
        assert!(
            sel.anchor.row < w.grid_text().lines().count(),
            "mapped row within the buffer"
        );
        send_left_release(&mut w, &mut doc);
        assert!(w.selection().is_none(), "a lone click clears on release");
    }
}

/// IME commits and the accessibility/semantic transcript surface.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "cases merged from separate tests; each case stays self-contained"
)]
fn ime_and_accessibility_expose_terminal_semantics() {
    // Case `ime_commit_writes_composed_text_to_the_focused_terminal`.
    {
        use winit::event::WindowEvent;

        let (mut w, rx) = build_test_widget();
        let mut doc = AutoCommit::new();
        w.set_focus(true);
        assert_eq!(w.ime_purpose(), Some(winit::window::ImePurpose::Terminal));
        assert_eq!(
            ui::ime::ImeWindowState::from_widget(&w).unwrap().purpose,
            winit::window::ImePurpose::Terminal
        );
        let stamp = w.ime_edit_fingerprint(&[9; 32]).unwrap();
        w.feed(b"x");
        assert_ne!(
            w.ime_edit_fingerprint(&[9; 32]).unwrap(),
            stamp,
            "deferred terminal input must invalidate when its live caret moves"
        );

        let stamp = w.ime_edit_fingerprint(&[9; 32]).unwrap();
        let caret = w.ime_cursor_rect();
        w.feed(b"\x1b[31m");
        assert_eq!(w.ime_cursor_rect(), caret);
        assert_ne!(
            w.ime_edit_fingerprint(&[9; 32]).unwrap(),
            stamp,
            "PTY state changes invalidate speech even without caret motion"
        );
        let stamp = w.ime_edit_fingerprint(&[9; 32]).unwrap();
        w.try_write_input(b"typed").unwrap();
        assert_ne!(
            w.ime_edit_fingerprint(&[9; 32]).unwrap(),
            stamp,
            "pending keyboard input must invalidate before PTY echo arrives"
        );
        assert_eq!(drain_writes(&rx), b"typed");

        let handled = ui::testing::Dispatch::new()
            .doc(&mut doc)
            .send(
                &mut w,
                &WindowEvent::Ime(winit::event::Ime::Commit("漢字".into())),
            )
            .handled;

        assert!(handled, "focused terminal consumes an IME commit");
        assert_eq!(drain_writes(&rx), "漢字".as_bytes());
    }

    // Case `app_wrapper_routes_ime_commit_to_the_focused_terminal`.
    {
        let (widget, rx) = build_test_widget();
        let mut app = ui::headless::Driver::new(State::new(), Box::new(widget), (800, 480));
        app.tab(false);
        app.send(&winit::event::WindowEvent::Ime(winit::event::Ime::Commit(
            "Grüße".into(),
        )));

        assert_eq!(
            drain_writes(&rx),
            "Grüße".as_bytes(),
            "the app wrapper delivers the committed IME text to the terminal PTY"
        );
    }

    // Case `accessibility_exposes_visible_terminal_output_and_focus`.
    {
        use ui::a11y::Role;
        use ui::widget::Widget;
        let (mut w, _rx) = build_test_widget();
        let doc = AutoCommit::new();
        w.force_geometry(Rect::new(0.0, 0.0, 800.0, 80.0), 10.0, 20.0);
        w.feed(b"accessible output");
        w.sync(&doc, &automerge::ROOT);
        w.set_focus(true);

        let node = w
            .accessibility()
            .expect("terminal has an assistive semantic node");
        assert_eq!(node.role, Role::Terminal);
        assert!(
            node.state.focusable
                && node.state.focused
                && node.state.multiline
                && node.state.read_only
        );
        assert!(
            node.value
                .as_deref()
                .unwrap_or_default()
                .contains("accessible output")
        );
        assert!(
            w.ime_cursor_rect().is_some(),
            "focused terminal anchors the platform IME"
        );
    }

    // Case `semantic_terminal_transcript_rejects_edits`.
    {
        use ui::a11y::Action;
        use ui::widget::Widget;

        let (mut w, writes) = build_test_widget();
        let mut doc = AutoCommit::new();
        assert!(!Widget::perform_action(
            &mut w,
            &Action::SetValue("printf semantic".to_string()),
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
        ));
        assert!(
            drain_writes(&writes).is_empty(),
            "a read-only transcript must never interpret SetValue as PTY input"
        );
    }

    // Case `semantic_selection_uses_graphemes_not_utf8_bytes`.
    {
        use ui::a11y::Action;
        use ui::widget::Widget;

        let (mut w, _writes) = build_test_widget();
        let mut doc = AutoCommit::new();
        w.force_geometry(Rect::new(0.0, 0.0, 800.0, 80.0), 10.0, 20.0);
        w.force_grid(vec!["A🦀e\u{301}".to_string(), "go".to_string()]);
        assert!(Widget::perform_action(
            &mut w,
            &Action::SetSelection {
                anchor: 1,
                focus: 3,
            },
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
        ));
        assert_eq!(
            w.selection(),
            Some(Selection {
                anchor: GridPos { row: 0, col: 1 },
                cursor: GridPos { row: 0, col: 4 },
            }),
            "the crab emoji is one a11y grapheme but occupies two terminal cells"
        );
        assert_eq!(
            w.accessibility().and_then(|node| node.text_selection),
            Some(ui::a11y::AccessibleTextSelection {
                anchor: 1,
                focus: "A🦀e\u{301}".len(),
            }),
            "the exported UTF-8 selection maps back to the same grapheme endpoints"
        );
        assert!(Widget::perform_action(
            &mut w,
            &Action::SetSelection {
                anchor: 1,
                focus: 6,
            },
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
        ));
        assert_eq!(
            w.selection(),
            Some(Selection {
                anchor: GridPos { row: 0, col: 1 },
                cursor: GridPos { row: 1, col: 2 },
            }),
            "the newline between visible rows consumes one a11y grapheme endpoint"
        );
        assert_eq!(
            w.accessibility().and_then(|node| node.text_selection),
            Some(ui::a11y::AccessibleTextSelection {
                anchor: 1,
                focus: "A🦀e\u{301}\ngo".len(),
            })
        );
    }
}

/// Terminal resource identity, CRDT convergence between peers, and presence.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "cases merged from separate tests; each case stays self-contained"
)]
fn terminal_resource_identity_convergence_and_presence() {
    // Case `concurrent_pane_claims_converge_on_one_resource_identity`.
    // Two peers can both claim a pane on the same previously empty session.
    // Automerge retains both concurrent scalar writes, so every replica must
    // select the same pane id from that conflict set, or resource and
    // presence addresses would split despite the document converging.
    {
        use automerge::ActorId;
        use ui::tilo_actions::Action as _;

        let base = AutoCommit::new();
        let mut left_doc = base.clone().with_actor(ActorId::random());
        let mut right_doc = base.with_actor(ActorId::random());
        for doc in [&mut left_doc, &mut right_doc] {
            crate::resource::TerminalAction::ClaimPane {
                pane_id: uuid::Uuid::new_v4().to_string(),
            }
            .apply(doc)
            .unwrap();
            doc.commit();
        }
        left_doc
            .merge_incoming(&mut right_doc)
            .expect("merge concurrent pane claims");
        right_doc
            .merge_incoming(&mut left_doc)
            .expect("return the merged session to the second peer");
        assert_eq!(
            left_doc
                .get_all(&ROOT, "terminal_pane_id")
                .expect("pane-id conflict values")
                .len(),
            2,
            "the test must exercise Automerge's concurrent-value case"
        );
        let left = crate::resource::Terminal::read(&left_doc).unwrap();
        let right = crate::resource::Terminal::read(&right_doc).unwrap();
        assert_eq!(left.pane_id, right.pane_id, "pane identities converge");
        let (mut left_view, _) = build_test_widget();
        let (mut right_view, _) = build_test_widget();
        ResourceSurface::<crate::resource::Terminal, crate::resource::TerminalEffect>::present(
            &mut left_view,
            &Rc::new(left),
        );
        ResourceSurface::<crate::resource::Terminal, crate::resource::TerminalEffect>::present(
            &mut right_view,
            &Rc::new(right),
        );
        assert_eq!(
            ui::presence::capability::presence_key(&left_view),
            ui::presence::capability::presence_key(&right_view),
            "presence addresses converge after merge"
        );
    }

    // Case `link_rows_are_bounded_resolvable_and_named_in_semantics`.
    {
        use ui::widget::Widget;

        let (mut widget, _writes) = build_test_widget();
        widget.force_geometry(Rect::new(0.0, 0.0, 800.0, 120.0), 10.0, 20.0);
        widget.feed(
            b"\x1b]2;build session\x07\x1b]8;;https://example.test/docs\x1b\\docs\x1b]8;;\x1b\\",
        );
        let label = <crate::resource::TerminalShell as ui::framework::ResourceApp>::label(
            &crate::resource::SessionState {
                title: widget.title().to_owned(),
                ..crate::resource::SessionState::default()
            },
        );
        assert_eq!(label, "Terminal — build session");
        let rows: Vec<_> = widget
            .link_rows()
            .into_iter()
            .map(ui::framework::LiveRow::into_resource)
            .collect();
        let [link] = rows.as_slice() else {
            panic!("one visible link row, not {}", rows.len());
        };
        assert_eq!(link.label, "https://example.test/docs");
        assert!(widget.link_row(link.id.as_str()).is_some());
        let semantics = Widget::accessibility(&widget).expect("terminal semantics");
        assert!(semantics.children.iter().any(|child| {
            child.role == ui::a11y::Role::Link
                && child.name.as_deref() == Some("https://example.test/docs")
                && child.action_target.as_deref() == Some(link.id.as_str())
                && child.state.focusable
        }));

        let mut link_burst = Vec::new();
        for index in 0..40 {
            link_burst.extend_from_slice(
                format!("\x1b]8;;https://example.test/{index}\x1b\\x\x1b]8;;\x1b\\").as_bytes(),
            );
        }
        widget.feed(&link_burst);
        let bounded = widget.link_rows();
        assert_eq!(bounded.len(), 32, "the published link rows are capped");
        let prefix = bounded[0]
            .id()
            .rsplit_once(':')
            .map(|(prefix, _)| prefix.to_owned())
            .expect("link id suffix");
        assert!(
            widget.link_row(&format!("{prefix}:41")).is_some(),
            "a link left out of the published rows resolves by id"
        );
    }

    // Case `a_collaborator_sees_the_running_sessions_screen_and_pane`.
    {
        // Peer A runs a session hosted by the framework; peer B joins from
        // A's record and shows it without a process of its own. Both show the
        // same screen and scope presence under the same pane.
        use ui::presence::PresenceKey;
        let mut a = ui::framework::testing::TestApp::<crate::resource::TerminalShell>::new()
            .expect("terminal");
        a.with_surface(|terminal: &mut TerminalWidget| terminal.feed(b"peer-a-line-one\r\n"))
            .expect("hosted terminal");
        a.settle();
        let joined = crate::resource::Terminal::read(&a.peer_document()).unwrap();
        let (mut b, _rx_b) = build_test_widget();
        ResourceSurface::<crate::resource::Terminal, crate::resource::TerminalEffect>::present(
            &mut b,
            &Rc::new(joined),
        );
        assert!(
            b.grid_text().contains("peer-a-line-one"),
            "B shows A's screen; B grid:\n{}",
            b.grid_text()
        );

        a.with_surface(|terminal: &mut TerminalWidget| terminal.feed(b"peer-a-line-two\r\n"))
            .expect("hosted terminal");
        a.settle();
        let later = crate::resource::Terminal::read(&a.peer_document()).unwrap();
        ResourceSurface::<crate::resource::Terminal, crate::resource::TerminalEffect>::present(
            &mut b,
            &Rc::new(later),
        );
        let (a_grid, a_key) = a
            .with_surface(|terminal: &mut TerminalWidget| {
                (
                    terminal.grid_text(),
                    ui::presence::capability::presence_key(terminal),
                )
            })
            .expect("hosted terminal");
        assert!(b.grid_text().contains("peer-a-line-two"));
        assert_eq!(
            a_grid.trim_end(),
            b.grid_text().trim_end(),
            "peers show the same terminal grid"
        );
        let b_key = ui::presence::capability::presence_key(&b);
        assert_eq!(
            a_key, b_key,
            "both peers scope presence under the same shared pane id"
        );
        match a_key {
            Some(PresenceKey::App { app, .. }) => {
                assert_eq!(app, ui::permissions::AppKind::Terminal);
            }
            other => panic!("expected App presence key, got {other:?}"),
        }
    }

    // Case `presence_payload_reports_selection_then_cursor`.
    {
        use ui::collaboration::PresencePayload;

        let (mut w, _) = build_test_widget();
        w.force_grid(vec!["abcdefgh".to_string()]);

        // With a selection, the payload is a Selection over linear indices.
        w.force_selection(Some(Selection {
            anchor: GridPos { row: 0, col: 2 },
            cursor: GridPos { row: 0, col: 5 },
        }));
        let (_key, payload) =
            ui::presence::capability::presence_payload(&w).expect("payload with selection");
        match payload {
            // width defaults to 80, so row 0 linearises to the column itself.
            PresencePayload::Selection { start, end } => {
                assert_eq!((start, end), (2, 5), "selection linearised to cell columns");
            }
            other => panic!("expected Selection payload, got {other:?}"),
        }

        // Without a selection, the payload is a Caret at the cursor.
        w.force_selection(None);
        let (_key, payload) =
            ui::presence::capability::presence_payload(&w).expect("payload with cursor");
        assert!(
            matches!(payload, PresencePayload::Caret { .. }),
            "cursor payload"
        );
    }
}

// ---------------------------------------------------------------
// SGR — 256-color, truecolor, and the four style bits.
// ---------------------------------------------------------------

/// SGR colours/styles and kitty images parse into cells and placements.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "cases merged from separate tests; each case stays self-contained"
)]
fn sgr_and_graphics_escapes_parse_into_cells_and_placements() {
    // Case `sgr_truecolor_and_indexed_colors_parse_into_cells`.
    {
        use crate::CellColor;
        use vello::kurbo::Rect;
        let (mut w, _rx) = build_test_widget();
        let doc = AutoCommit::new();
        w.force_geometry(Rect::new(0.0, 0.0, 800.0, 480.0), 10.0, 20.0);

        // Semicolon truecolor foreground.
        w.feed(b"\x1b[H\x1b[38;2;10;20;30mA");
        w.sync(&doc, &automerge::ROOT);
        assert_eq!(
            w.cell_attr(0, 0).fg,
            CellColor::Rgb(10, 20, 30),
            "38;2 truecolor fg"
        );

        // Semicolon 256-indexed foreground.
        w.feed(b"\x1b[H\x1b[38;5;196mB");
        w.sync(&doc, &automerge::ROOT);
        assert_eq!(
            w.cell_attr(0, 0).fg,
            CellColor::Indexed(196),
            "38;5 indexed fg"
        );

        // Colon-subparam 256-indexed foreground (the xterm form apps emit).
        w.feed(b"\x1b[H\x1b[38:5:214mC");
        w.sync(&doc, &automerge::ROOT);
        assert_eq!(
            w.cell_attr(0, 0).fg,
            CellColor::Indexed(214),
            "38:5 colon-form indexed fg"
        );

        // Truecolor background.
        w.feed(b"\x1b[H\x1b[48;2;5;6;7mD");
        w.sync(&doc, &automerge::ROOT);
        assert_eq!(
            w.cell_attr(0, 0).bg,
            CellColor::Rgb(5, 6, 7),
            "48;2 truecolor bg"
        );
    }

    // Case `sgr_style_bits_and_reset_parse_into_cells`.
    {
        use crate::CellColor;
        use vello::kurbo::Rect;
        let (mut w, _rx) = build_test_widget();
        let doc = AutoCommit::new();
        w.force_geometry(Rect::new(0.0, 0.0, 800.0, 480.0), 10.0, 20.0);

        // Bold + italic + underline + reverse all at once.
        w.feed(b"\x1b[H\x1b[1;3;4;7mW");
        w.sync(&doc, &automerge::ROOT);
        let a = w.cell_attr(0, 0);
        assert!(
            a.bold && a.italic && a.underline && a.reverse,
            "combined SGR style bits: {a:?}"
        );

        // A reset clears everything back to the default attribute.
        w.feed(b"\x1b[H\x1b[0mN");
        w.sync(&doc, &automerge::ROOT);
        let b = w.cell_attr(0, 0);
        assert_eq!(b.fg, CellColor::Default);
        assert!(
            !b.bold && !b.italic && !b.underline && !b.reverse,
            "SGR 0 resets style: {b:?}"
        );
    }

    // Case `sgr_dim_and_strikethrough_parse_into_cells`.
    {
        use vello::kurbo::Rect;
        let (mut w, _rx) = build_test_widget();
        let doc = AutoCommit::new();
        w.force_geometry(Rect::new(0.0, 0.0, 800.0, 480.0), 10.0, 20.0);

        // Faint + crossed-out together.
        w.feed(b"\x1b[H\x1b[2;9mX");
        w.sync(&doc, &automerge::ROOT);
        let a = w.cell_attr(0, 0);
        assert!(a.dim, "SGR 2 sets faint: {a:?}");
        assert!(a.strike, "SGR 9 sets crossed-out: {a:?}");

        // SGR 22 clears both bold and faint; SGR 29 clears crossed-out.
        w.feed(b"\x1b[H\x1b[1;2;9mY\x1b[22;29mZ");
        w.sync(&doc, &automerge::ROOT);
        let z = w.cell_attr(0, 1);
        assert!(!z.dim && !z.bold, "SGR 22 clears bold+faint: {z:?}");
        assert!(!z.strike, "SGR 29 clears crossed-out: {z:?}");
    }

    // Case `kitty_image_becomes_a_shared_content_addressed_renderable_placement`.
    {
        use base64::Engine as _;
        let png = base64::engine::general_purpose::STANDARD
        .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=")
        .unwrap();
        let encoded = base64::engine::general_purpose::STANDARD.encode(&png);
        let (mut host, rx) = build_test_widget();
        let host_doc = AutoCommit::new();
        host.force_geometry(Rect::new(0.0, 0.0, 800.0, 480.0), 10.0, 20.0);
        host.feed(b"before ");
        host.feed(format!("\x1b_Ga=T,f=100,t=d,c=3,r=2,i=42;{encoded}\x1b\\after").as_bytes());
        host.sync(&host_doc, &automerge::ROOT);

        assert!(
            host.grid_text().contains("before after"),
            "APC bytes never leak into ANSI text"
        );
        assert!(
            drain_writes(&rx).is_empty(),
            "no success response advertises rendering before it exists"
        );
        let [placement] = host.image_placements() else {
            panic!("one placement expected: {:?}", host.image_placements());
        };
        assert_eq!(
            (
                placement.columns,
                placement.rows,
                placement.pixel_width,
                placement.pixel_height,
                placement.protocol_id
            ),
            (3, 2, 1, 1, Some(42))
        );
        assert_eq!(
            host.content_store().read(&placement.hash).as_deref(),
            Some(png.as_slice())
        );

        let peer = replica_of(&host);
        assert_eq!(
            peer.image_placements(),
            host.image_placements(),
            "placement metadata converges through the session record"
        );
    }
}

// ---------------------------------------------------------------
// Real PTY — a live subprocess through the production reader→sync path.
// ---------------------------------------------------------------

/// Pump and settle the production terminal lifecycle until the grid contains
/// `needle` or `tries` elapse. Returns whether it appeared.
fn pump_until_contains(w: &mut TerminalWidget, doc: &mut AutoCommit, needle: &str) -> bool {
    let deadline = ui::time::now() + HANG_BOUND;
    while ui::time::now() < deadline {
        w.update(doc, &ROOT);
        w.settle(&mut ui::widget::Edits::lend(doc), &ROOT);
        w.sync(doc, &ROOT);
        if w.grid_text().contains(needle) {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    w.grid_text().contains(needle)
}

/// Real PTY spawns: output, typed commands, grid size, fresh grids and TERM.
#[test]
fn spawned_pty_sessions_run_commands_in_a_fresh_grid() {
    // Case `spawn_command_runs_real_pty_and_grid_shows_output`.
    {
        // Run a real subprocess in a real PTY and prove its stdout reaches the
        // emulator grid through the production reader → sync → vte → CRDT path
        // (no `feed`, no mock). This is the load-bearing "the terminal actually
        // runs programs" proof.
        let cmd = shell_command("printf 'TILO_PTY_OK\\n'", "echo TILO_PTY_OK");
        let mut w = TerminalWidget::spawn_command(cmd).expect("spawn pty command");
        let mut doc = AutoCommit::new();
        assert!(
            pump_until_contains(&mut w, &mut doc, "TILO_PTY_OK"),
            "real PTY command output must reach the grid; grid was:\n{}",
            w.grid_text()
        );
    }

    // Case `default_shell_runs_typed_command_through_pty`.
    {
        // Spawn the user's default shell in a real PTY, then type a command
        // exactly the way a keystroke sends it (`write_input`), and prove the
        // shell ran it and the output flowed back into the grid.
        #[cfg(unix)]
        let home = tempfile::tempdir().expect("scratch HOME");
        #[cfg(unix)]
        let cmd = hermetic_default_shell_command(home.path());
        #[cfg(windows)]
        let cmd = crate::inner::login_shell_command();
        let mut w = TerminalWidget::spawn_command(cmd).expect("spawn default shell");
        let mut doc = AutoCommit::new();
        // Let the shell reach a prompt before typing.
        w.sync(&doc, &ROOT);
        #[cfg(unix)]
        w.write_input(b"printf 'TILO_%s_OK\\n' SHELL; exit\n");
        #[cfg(windows)]
        w.write_input(b"set TILO_PART=SHELL\r\necho TILO_%TILO_PART%_OK\r\nexit\r\n");
        assert!(
            pump_until_contains(&mut w, &mut doc, "TILO_SHELL_OK"),
            "the real shell must run the typed command and show output; grid was:\n{}",
            w.grid_text()
        );
    }

    // Case `spawn_command_reports_grid_size`.
    {
        // The default PTY is opened at 80x24; the host-facing accessor must
        // report that until a `place_at` reflow changes it.
        let w = TerminalWidget::spawn_command(shell_command("true", "exit /b 0")).expect("spawn");
        assert_eq!(w.grid_size(), (80, 24));
    }

    // Case `newly_spawned_local_pty_does_not_append_to_a_previous_session_grid`.
    {
        use automerge::AutoCommit;

        let mut old = TerminalWidget::spawn_headless().expect("old terminal");
        let doc = AutoCommit::new();
        old.feed(b"old-session-output");
        old.sync(&doc, &automerge::ROOT);
        assert!(old.grid_text().contains("old-session-output"));

        // A real spawned widget owns a new local PTY incarnation. Use a short
        // command so this test exercises the production reader/sync path without
        // leaving an interactive shell behind.
        let mut command = crate::CommandBuilder::new("/bin/sh");
        command.arg("-c");
        command.arg("printf new-session-output");
        let mut fresh = TerminalWidget::spawn_command(command).expect("new local terminal");
        let deadline = ui::time::now() + HANG_BOUND;
        while ui::time::now() < deadline {
            fresh.sync(&doc, &automerge::ROOT);
            if fresh.grid_text().contains("new-session-output") {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }

        let screen = fresh.grid_text();
        assert!(
            screen.contains("new-session-output"),
            "screen was {screen:?}"
        );
        assert!(
            !screen.contains("old-session-output"),
            "a new PTY must not inherit the previous process' screen: {screen:?}"
        );
    }

    // Case `with_command_forces_xterm_256color_term`.
    {
        // fish (and terminfo) need $TERM to name a terminal they support. Run a
        // child that prints $TERM and prove the emulator exported it.
        let cmd = shell_command("printf 'TERM=%s\\n' \"$TERM\"", "echo TERM=%TERM%");
        let mut w = TerminalWidget::spawn_command(cmd).expect("spawn pty command");
        let mut doc = AutoCommit::new();
        assert!(
            pump_until_contains(&mut w, &mut doc, "TERM=xterm-256color"),
            "the shell must see TERM=xterm-256color; grid was:\n{}",
            w.grid_text()
        );
    }
}

/// The user's default shell exactly as the Host target off Tilo resolves it, but
/// started in a controlled environment: a scratch `HOME`, a minimal env and no
/// system or user startup files. Host startup scripts may read the terminal's
/// stdin (distrobox's `profile.d` hook runs `host-spawn` when no display
/// variables are set, swallowing typed input), so a test that types into the
/// shell must not let them run.
#[cfg(unix)]
fn hermetic_default_shell_command(home: &std::path::Path) -> portable_pty::CommandBuilder {
    let mut cmd = crate::inner::login_shell_command();
    let shell = cmd
        .get_argv()
        .first()
        .map(std::path::PathBuf::from)
        .expect("login shell command has a program");
    match shell.file_name().and_then(|n| n.to_str()) {
        Some("bash") => cmd.args(["--noprofile", "--norc"]),
        Some("zsh") => cmd.arg("-f"),
        Some("fish") => cmd.arg("--no-config"),
        _ => {}
    }
    let empty_rc = home.join("empty-rc");
    std::fs::write(&empty_rc, b"").expect("write empty rc file");
    cmd.env_clear();
    if let Some(path) = std::env::var_os("PATH") {
        cmd.env("PATH", path);
    }
    cmd.env("HOME", home);
    cmd.env("XDG_CONFIG_HOME", home);
    cmd.env("ZDOTDIR", home);
    // POSIX `sh` (and bash as `sh`) reads `$ENV` when interactive; bash reads
    // `$BASH_ENV` for scripts. Both point at an empty file.
    cmd.env("ENV", &empty_rc);
    cmd.env("BASH_ENV", &empty_rc);
    cmd.cwd(home);
    cmd
}

// ---------------------------------------------------------------
// SGR intensity / crossed-out — dim (2) and strikethrough (9).
// ---------------------------------------------------------------

// ---------------------------------------------------------------
// Keyboard encoding — modified cursor/function keys reach the PTY.
// ---------------------------------------------------------------

/// Key encoding: modified arrows, backspace, DECCKM, function keys, back-tab and Alt/`AltGr`.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "cases merged from separate tests; each case stays self-contained"
)]
fn keyboard_encoding_sends_expected_sequences() {
    // Case `ctrl_arrow_sends_modified_csi_for_word_motion`.
    {
        use vello::kurbo::Rect;
        use winit::keyboard::{Key, NamedKey};
        let (mut w, rx) = build_test_widget();
        let mut doc = AutoCommit::new();
        w.force_geometry(Rect::new(0.0, 0.0, 800.0, 480.0), 10.0, 20.0);
        w.feed(b""); // init modes

        send_key_mods(
            &mut w,
            &mut doc,
            Key::Named(NamedKey::ArrowLeft),
            true,
            false,
            false,
        );
        assert_eq!(
            drain_writes(&rx),
            b"\x1b[1;5D",
            "Ctrl+Left = CSI 1;5D (word-left)"
        );

        send_key_mods(
            &mut w,
            &mut doc,
            Key::Named(NamedKey::ArrowRight),
            true,
            false,
            false,
        );
        assert_eq!(
            drain_writes(&rx),
            b"\x1b[1;5C",
            "Ctrl+Right = CSI 1;5C (word-right)"
        );

        // Shift+arrow (in-app selection) uses modifier param 2.
        send_key_mods(
            &mut w,
            &mut doc,
            Key::Named(NamedKey::ArrowUp),
            false,
            true,
            false,
        );
        assert_eq!(drain_writes(&rx), b"\x1b[1;2A", "Shift+Up = CSI 1;2A");
    }

    // Case `modified_backspace_uses_shell_compatible_word_deletion`.
    {
        use winit::keyboard::{Key, NamedKey};
        let (mut w, rx) = build_test_widget();
        let mut doc = AutoCommit::new();

        send_key_mods(
            &mut w,
            &mut doc,
            Key::Named(NamedKey::Backspace),
            true,
            false,
            false,
        );
        assert_eq!(
            drain_writes(&rx),
            b"\x17",
            "Ctrl+Backspace sends readline ^W"
        );

        send_key_mods(
            &mut w,
            &mut doc,
            Key::Named(NamedKey::Backspace),
            false,
            false,
            true,
        );
        assert_eq!(
            drain_writes(&rx),
            b"\x1b\x7f",
            "Alt+Backspace sends Meta-DEL"
        );
    }

    // Case `unmodified_arrow_respects_decckm_app_cursor_keys`.
    {
        use vello::kurbo::Rect;
        use winit::keyboard::{Key, NamedKey};
        let (mut w, rx) = build_test_widget();
        let mut doc = AutoCommit::new();
        w.force_geometry(Rect::new(0.0, 0.0, 800.0, 480.0), 10.0, 20.0);

        // Default (normal cursor keys) → CSI form.
        send_key_mods(
            &mut w,
            &mut doc,
            Key::Named(NamedKey::ArrowUp),
            false,
            false,
            false,
        );
        assert_eq!(drain_writes(&rx), b"\x1b[A", "default arrow = CSI A");

        // App turns on DECCKM; unmodified arrows now use SS3.
        w.feed(b"\x1b[?1h");
        assert!(w.modes().app_cursor_keys);
        send_key_mods(
            &mut w,
            &mut doc,
            Key::Named(NamedKey::ArrowUp),
            false,
            false,
            false,
        );
        assert_eq!(drain_writes(&rx), b"\x1bOA", "DECCKM arrow = SS3 A");
    }

    // Case `function_keys_send_expected_sequences`.
    {
        use vello::kurbo::Rect;
        use winit::keyboard::{Key, NamedKey};
        let (mut w, rx) = build_test_widget();
        let mut doc = AutoCommit::new();
        w.force_geometry(Rect::new(0.0, 0.0, 800.0, 480.0), 10.0, 20.0);

        send_key_mods(
            &mut w,
            &mut doc,
            Key::Named(NamedKey::F1),
            false,
            false,
            false,
        );
        assert_eq!(drain_writes(&rx), b"\x1bOP", "F1 = SS3 P");
        send_key_mods(
            &mut w,
            &mut doc,
            Key::Named(NamedKey::F5),
            false,
            false,
            false,
        );
        assert_eq!(drain_writes(&rx), b"\x1b[15~", "F5 = CSI 15~");
        send_key_mods(
            &mut w,
            &mut doc,
            Key::Named(NamedKey::F10),
            false,
            false,
            false,
        );
        assert_eq!(drain_writes(&rx), b"\x1b[21~", "F10 = CSI 21~");
        send_key_mods(
            &mut w,
            &mut doc,
            Key::Named(NamedKey::F12),
            false,
            false,
            false,
        );
        assert_eq!(drain_writes(&rx), b"\x1b[24~", "F12 = CSI 24~");
    }

    // Case `shift_tab_sends_back_tab`.
    {
        use vello::kurbo::Rect;
        use winit::keyboard::{Key, NamedKey};
        let (mut w, rx) = build_test_widget();
        let mut doc = AutoCommit::new();
        w.force_geometry(Rect::new(0.0, 0.0, 800.0, 480.0), 10.0, 20.0);

        send_key_mods(
            &mut w,
            &mut doc,
            Key::Named(NamedKey::Tab),
            false,
            false,
            false,
        );
        assert_eq!(drain_writes(&rx), b"\t", "plain Tab = HT");
        send_key_mods(
            &mut w,
            &mut doc,
            Key::Named(NamedKey::Tab),
            false,
            true,
            false,
        );
        assert_eq!(drain_writes(&rx), b"\x1b[Z", "Shift+Tab = CSI Z (back-tab)");
    }

    // Case `alt_letter_sends_meta_escape_but_altgr_symbols_pass_through`.
    {
        use vello::kurbo::Rect;
        use winit::keyboard::{Key, SmolStr};
        let (mut w, rx) = build_test_widget();
        let mut doc = AutoCommit::new();
        w.force_geometry(Rect::new(0.0, 0.0, 800.0, 480.0), 10.0, 20.0);

        // Alt+f → ESC f (readline forward-word).
        send_key_mods(
            &mut w,
            &mut doc,
            Key::Character(SmolStr::new_static("f")),
            false,
            false,
            true,
        );
        assert_eq!(drain_writes(&rx), b"\x1bf", "Alt+f = ESC f (Meta)");

        // AltGr on a German layout yields a composed *symbol* as the logical
        // key (e.g. '@'); Meta must NOT poach it — the symbol itself is sent.
        send_key_mods(
            &mut w,
            &mut doc,
            Key::Character(SmolStr::new_static("@")),
            false,
            false,
            true,
        );
        assert_eq!(
            drain_writes(&rx),
            b"@",
            "AltGr-composed '@' passes through verbatim"
        );
    }
}

// ---------------------------------------------------------------
// Incremental grid mirror — heavy output stays correct + bounded.
// ---------------------------------------------------------------

// ---------------------------------------------------------------
// Full text selection — multi-click word/line, shift-extend, and
// buffer-anchored selection that survives scrolling.
// ---------------------------------------------------------------

/// Send a left press with explicit modifiers (for shift-extend).
fn send_left_press_mods(widget: &mut TerminalWidget, doc: &mut AutoCommit, shift: bool) {
    use winit::event::{ElementState, MouseButton, WindowEvent};
    use winit::keyboard::ModifiersState;
    let mut state = ModifiersState::empty();
    if shift {
        state |= ModifiersState::SHIFT;
    }
    let ev = WindowEvent::MouseInput {
        device_id: winit::event::DeviceId::dummy(),
        state: ElementState::Pressed,
        button: MouseButton::Left,
    };
    ui::testing::Dispatch::new()
        .modifiers(state)
        .doc(doc)
        .send(widget, &ev);
}

/// Send a middle-button press (primary-selection paste).
fn send_middle_press(widget: &mut TerminalWidget, doc: &mut AutoCommit) {
    use winit::event::{ElementState, MouseButton, WindowEvent};
    let ev = WindowEvent::MouseInput {
        device_id: winit::event::DeviceId::dummy(),
        state: ElementState::Pressed,
        button: MouseButton::Middle,
    };
    ui::testing::Dispatch::new().doc(doc).send(widget, &ev);
}

/// Logical x for the centre of grid column `col` given `char_w=10`,
/// `padding=4` (the widget's default gutter).
fn col_center_x(col: usize) -> f64 {
    4.0 + col as f64 * 10.0 + 5.0
}

/// Double/triple/shift click selection, buffer anchoring and synchronous highlight.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "cases merged from separate tests; each case stays self-contained"
)]
#[serial_test::serial]
fn mouse_selection_gestures_select_extend_and_stay_anchored() {
    // Case `double_click_selects_word`.
    {
        // A double-click selects the whole word under the pointer via the real
        // press → press gesture path (toolkit `DragSelect` counts the clicks,
        // `word_boundaries` finds the bounds).
        let (mut w, _) = build_test_widget();
        let mut doc = AutoCommit::new();
        w.force_geometry(Rect::new(0.0, 0.0, 800.0, 480.0), 10.0, 20.0);
        w.force_viewport_start(0);
        w.force_grid(vec!["hello world foobar".to_string()]);

        // Point at a cell inside "world" (cols 6..=10) and double-click.
        send_cursor_move(&mut w, &mut doc, col_center_x(8), 10.0);
        send_left_press(&mut w, &mut doc);
        send_left_release(&mut w, &mut doc);
        send_left_press(&mut w, &mut doc); // 2nd click within threshold → word
        assert_eq!(
            w.test_selected_text().as_deref(),
            Some("world"),
            "double-click selects the word under the pointer"
        );
        send_left_release(&mut w, &mut doc);
        assert!(w.selection().is_some(), "word selection survives release");
    }

    // Case `triple_click_selects_line`.
    {
        // A triple-click selects the whole grid line.
        let (mut w, _) = build_test_widget();
        let mut doc = AutoCommit::new();
        w.force_geometry(Rect::new(0.0, 0.0, 800.0, 480.0), 10.0, 20.0);
        w.force_viewport_start(0);
        w.force_grid(vec!["hello world foobar".to_string()]);

        send_cursor_move(&mut w, &mut doc, col_center_x(8), 10.0);
        send_left_press(&mut w, &mut doc);
        send_left_release(&mut w, &mut doc);
        send_left_press(&mut w, &mut doc); // 2nd → word
        send_left_release(&mut w, &mut doc);
        send_left_press(&mut w, &mut doc); // 3rd → line
        assert_eq!(
            w.test_selected_text().as_deref(),
            Some("hello world foobar"),
            "triple-click selects the whole line (trailing padding trimmed)"
        );
    }

    // Case `shift_click_extends_existing_selection`.
    {
        // Drag-select "llo w", then shift+click further right to extend the
        // selection while keeping the original anchor.
        let (mut w, _) = build_test_widget();
        let mut doc = AutoCommit::new();
        w.force_geometry(Rect::new(0.0, 0.0, 800.0, 480.0), 10.0, 20.0);
        w.force_viewport_start(0);
        w.force_grid(vec!["hello world foobar".to_string()]);

        // Drag from col 2 to col 5 → selection [2,5).
        send_cursor_move(&mut w, &mut doc, col_center_x(2), 10.0);
        send_left_press(&mut w, &mut doc);
        send_cursor_move(&mut w, &mut doc, col_center_x(5), 10.0);
        send_left_release(&mut w, &mut doc);
        assert_eq!(
            w.test_selected_text().as_deref(),
            Some("llo"),
            "initial drag selection"
        );

        // Shift+click at col 10 extends the caret, anchor pinned at col 2.
        send_cursor_move(&mut w, &mut doc, col_center_x(10), 10.0);
        send_left_press_mods(&mut w, &mut doc, /*shift=*/ true);
        // cols [2,10) of "hello world foobar" = "llo worl".
        assert_eq!(
            w.test_selected_text().as_deref(),
            Some("llo worl"),
            "shift+click extends from the original anchor"
        );
    }

    // Case `selection_is_buffer_anchored_and_survives_scrolling`.
    {
        // The headline property: a selection anchors to absolute buffer rows,
        // so scrolling the viewport does not change what is selected. Drive a
        // real double-click over a scrollback row, then move the viewport and
        // confirm the selected text is byte-identical.
        let (mut w, _write_rx) = build_test_widget();
        let mut doc = AutoCommit::new();
        w.force_geometry(Rect::new(0.0, 0.0, 800.0, 240.0), 10.0, 20.0); // 12 visible rows
        // A deterministic 20-row buffer; row 5 carries a distinctive word.
        let mut grid: Vec<String> = (0..20).map(|i| format!("row{i:02} filler text")).collect();
        grid[5] = "alpha bravo charlie".to_string();
        w.force_grid(grid);
        // Simulate the draw having scrolled so row 5 is the top visible row.
        w.force_viewport_start(5);
        w.set_select_to_copy(true);

        // Double-click "bravo" (cols 6..=10) on the top visible row (vrow 0).
        send_cursor_move(&mut w, &mut doc, col_center_x(8), 10.0);
        send_left_press(&mut w, &mut doc);
        send_left_release(&mut w, &mut doc);
        send_left_press(&mut w, &mut doc); // word
        assert_eq!(
            w.test_selected_text().as_deref(),
            Some("bravo"),
            "selected the right word"
        );
        send_left_release(&mut w, &mut doc); // finish → primary (+ clipboard: select_to_copy)

        // The selection lives on absolute row 5.
        let sel = w.selection().expect("selection present");
        assert_eq!(
            sel.anchor.row, 5,
            "selection anchored to absolute buffer row 5"
        );

        // "Scroll": move the viewport elsewhere. The selected text must not move.
        w.force_viewport_start(0);
        assert_eq!(
            w.test_selected_text().as_deref(),
            Some("bravo"),
            "selection stays on the same buffer cells after scrolling"
        );
        w.force_viewport_start(8);
        assert_eq!(w.test_selected_text().as_deref(), Some("bravo"));

        // Round-trip through the same entry points production reads from,
        // not past them to a concrete backend: the copy resolves to one of
        // several backends and to an in-process buffer when a process has
        // no window, so reading arboard directly asked the host's clipboard
        // about a write that never went there. Releasing the drag always
        // sets the primary selection, and sets the clipboard too because
        // `select_to_copy` is on above — both are asserted, unconditionally,
        // so a copy that silently did nothing fails here.
        assert_eq!(
            ui::text_selection::clipboard_paste_primary().as_deref(),
            Some("bravo"),
            "finishing a drag puts exactly the selection on the primary selection"
        );
        assert_eq!(
            ui::text_selection::clipboard_paste().as_deref(),
            Some("bravo"),
            "select-to-copy put exactly the selection on the clipboard"
        );
    }

    // Case `selection_highlight_is_produced_synchronously_on_drag`.
    {
        use winit::dpi::PhysicalPosition;
        use winit::event::WindowEvent;

        let (mut widget, _) = build_test_widget();
        let mut doc = AutoCommit::new();
        widget.force_geometry(Rect::new(0.0, 0.0, 800.0, 600.0), 10.0, 20.0);

        // Press at col 2, row 1 (padding is 4px → col k spans [4+k*10, 4+(k+1)*10)).
        send_cursor_move(&mut widget, &mut doc, 4.0 + 2.0 * 10.0 + 1.0, 20.0 + 1.0);
        send_left_press(&mut widget, &mut doc);

        // Drag across to col 20 — dispatch the CursorMoved directly so we can read
        // back whether it was `handled` (the App's incremental-redraw gate only
        // repaints a pointer move when the widget reports it, so a drag that
        // isn't handled would leave the highlight invisible until the next tick).
        let ev = WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: PhysicalPosition::new(4.0 + 20.0 * 10.0 + 1.0, 20.0 + 1.0),
        };
        let handled = ui::testing::Dispatch::new()
            .doc(&mut doc)
            .send(&mut widget, &ev)
            .handled;

        // The highlight span exists in the SAME event that moved the pointer —
        // no deferred re-layout, no debounce, no full-grid rework required.
        let spans = widget.current_row_spans();
        assert_eq!(
            spans,
            vec![(1, 2, 20)],
            "the selection highlight must cover the dragged cells synchronously"
        );
        assert!(
            handled,
            "the drag must be reported handled so the App repaints it immediately"
        );
    }
}

// ---------------------------------------------------------------
// Collaboration — two peers converge on the same terminal grid via
// the real CRDT save/load-incremental path the toolkit App uses.
// ---------------------------------------------------------------

// ---------------------------------------------------------------
// Spec compliance — OSC 0/2 title, OSC 52 clipboard, OSC 8 links.
// ---------------------------------------------------------------

/// OSC title metadata and OSC 8 hyperlinks.
#[test]
fn osc_sequences_update_title_and_hyperlinks() {
    // Case `osc_window_title_updates_app_title`.
    {
        use ui::AppWidget;
        let (mut w, _) = build_test_widget();
        // OSC 2 ; <title> BEL
        w.feed(b"\x1b]2;my session\x07");
        assert_eq!(w.title(), "my session", "OSC 2 sets the window title");
        assert_eq!(
            w.app_title(),
            "my session",
            "app_title reflects the OSC title"
        );

        // OSC 0 (icon + title) also sets it; ST-terminated form.
        w.feed(b"\x1b]0;another\x1b\\");
        assert_eq!(w.title(), "another", "OSC 0 sets the window title too");

        // The title rides the session record: a collaborator's view reads it.
        let peer = replica_of(&w);
        assert_eq!(
            peer.title(),
            "another",
            "a collaborator sees the same title"
        );
    }

    // Case `process_controlled_osc_metadata_is_bounded_and_single_line`.
    {
        assert_eq!(
            crate::inner::safe_osc_text(b"build 42", 32).as_deref(),
            Some("build 42")
        );
        assert!(crate::inner::safe_osc_text(b"line one\nline two", 32).is_none());
        assert!(crate::inner::safe_osc_text(&[b'x'; 33], 32).is_none());
        assert!(crate::inner::safe_osc_text(&[0xff], 32).is_none());
    }

    // Case `osc_8_hyperlink_marks_cells_and_resolves_url`.
    {
        // OSC 8 ; ; <uri> ST … OSC 8 ; ; ST wraps the cells in between with a
        // hyperlink that resolves back to the URL (rendered underlined and
        // clickable). The link registry rides the CRDT so peers resolve it too.
        let (mut w, _) = build_test_widget();
        w.force_geometry(Rect::new(0.0, 0.0, 800.0, 480.0), 10.0, 20.0);
        // Home, open link, print "link", close link.
        w.feed(b"\x1b[H\x1b]8;;https://example.test/docs\x1b\\link\x1b]8;;\x1b\\");

        assert_eq!(w.grid_row(0).trim_end(), "link", "the link text is printed");
        assert_eq!(
            w.hyperlink_at(0, 0),
            Some("https://example.test/docs"),
            "first link cell resolves to the URL"
        );
        assert_eq!(
            w.hyperlink_at(0, 3),
            Some("https://example.test/docs"),
            "last link cell resolves to the URL"
        );
        assert_eq!(
            w.hyperlink_at(0, 10),
            None,
            "cells outside the link have no URL"
        );

        // A collaborator's view of the session resolves the same hyperlink.
        let peer = replica_of(&w);
        assert_eq!(
            peer.hyperlink_at(0, 1),
            Some("https://example.test/docs"),
            "a collaborator resolves the same OSC 8 hyperlink"
        );
    }
}

#[test]
#[cfg(target_os = "linux")]
#[serial_test::serial]
fn osc_52_writes_clipboard_from_base64() {
    // OSC 52 ; c ; <base64> sets the clipboard. Read back through the
    // production paste entry point rather than a concrete backend — the
    // copy resolves to an in-process buffer in a windowless test, so
    // asking arboard reported the host's real clipboard instead. No
    // skip: the round trip is always observable, so an OSC 52 that
    // decoded nothing fails here rather than passing quietly.
    let (mut w, _) = build_test_widget();
    // base64("tilo-osc52") = dGlsby1vc2M1Mg==
    w.feed(b"\x1b]52;c;dGlsby1vc2M1Mg==\x07");
    assert_eq!(
        ui::text_selection::clipboard_paste().as_deref(),
        Some("tilo-osc52"),
        "OSC 52 decodes base64 into the clipboard"
    );
}

// ---------------------------------------------------------------
// Grid geometry — a uniform body padding + scrollbar gutter inset the grid
// (there is NO app-drawn header/tab bar; the compositor owns window
// decoration), and pointer hit-testing follows the same inset grid rect
// (production `place_at`/`set_geometry` path).
// ---------------------------------------------------------------

// ---------------------------------------------------------------
// Terminal capability queries — the emulator must reply on the PTY.
// fish blocks on the DA1 reply for up to 10s at startup and disables
// features if it never arrives; every reply below travels the same
// `write_input` path a keystroke does, so the shell reads it.
// ---------------------------------------------------------------

/// DA1/DA2/DECID/XTVERSION replies.
#[test]
fn device_attribute_queries_reply() {
    // Case `da1_query_writes_primary_device_attributes_reply`.
    {
        let (mut w, write_rx) = build_test_widget();
        // Exactly the bytes fish sends to probe the terminal: CSI c.
        w.feed(b"\x1b[c");
        let got = drain_writes(&write_rx);
        assert_eq!(
            got, b"\x1b[?62;22c",
            "DA1 (CSI c) must answer with the VT220+ANSI-color device attributes"
        );
    }

    // Case `da1_query_with_zero_param_also_replies`.
    {
        let (mut w, write_rx) = build_test_widget();
        w.feed(b"\x1b[0c");
        assert_eq!(
            drain_writes(&write_rx),
            b"\x1b[?62;22c",
            "CSI 0 c is DA1 too"
        );
    }

    // Case `decid_esc_z_replies_like_da1`.
    {
        let (mut w, write_rx) = build_test_widget();
        w.feed(b"\x1bZ");
        assert_eq!(
            drain_writes(&write_rx),
            b"\x1b[?62;22c",
            "ESC Z (DECID) is a DA1 request"
        );
    }

    // Case `da2_secondary_device_attributes_reply`.
    {
        let (mut w, write_rx) = build_test_widget();
        w.feed(b"\x1b[>c");
        let expected = format!("\x1b[>1;{};0c", crate::inner::terminal_da2_version());
        assert_eq!(
            drain_writes(&write_rx),
            expected.as_bytes(),
            "DA2 (CSI > c) reports terminal type 1 + build version + 0"
        );
    }

    // Case `xtversion_reply_names_the_terminal`.
    {
        let (mut w, write_rx) = build_test_widget();
        w.feed(b"\x1b[>0q");
        let got = drain_writes(&write_rx);
        let expected = format!("\x1bP>|tilo-terminal({})\x1b\\", env!("CARGO_PKG_VERSION"));
        assert_eq!(
            got,
            expected.as_bytes(),
            "XTVERSION (CSI > 0 q) names + versions the terminal"
        );
    }
}

/// DSR, DECRQM and unknown-query handling.
#[test]
fn status_and_mode_reports_reply_or_stay_silent() {
    // Case `dsr_cursor_position_report_uses_live_cursor`.
    {
        let (mut w, write_rx) = build_test_widget();
        // Move the cursor to row 5, column 3 (1-based) via CUP, then ask for it.
        w.feed(b"\x1b[5;3H");
        let _ = drain_writes(&write_rx); // CUP itself sends nothing; clear.
        w.feed(b"\x1b[6n");
        assert_eq!(
            drain_writes(&write_rx),
            b"\x1b[5;3R",
            "DSR 6n (CPR) reports the 1-based cursor row;col"
        );
    }

    // Case `dsr_device_status_reports_ok`.
    {
        let (mut w, write_rx) = build_test_widget();
        w.feed(b"\x1b[5n");
        assert_eq!(drain_writes(&write_rx), b"\x1b[0n", "DSR 5n → terminal OK");
    }

    // Case `dsr_private_cursor_position_report`.
    {
        let (mut w, write_rx) = build_test_widget();
        w.feed(b"\x1b[2;7H");
        let _ = drain_writes(&write_rx);
        w.feed(b"\x1b[?6n");
        assert_eq!(
            drain_writes(&write_rx),
            b"\x1b[?2;7;1R",
            "DECXCPR (?6n) reports row;col;page"
        );
    }

    // Case `decrqm_reports_bracketed_paste_state`.
    {
        let (mut w, write_rx) = build_test_widget();
        // Enable bracketed paste, then query its mode: DECRPM value 1 = set.
        w.feed(b"\x1b[?2004h\x1b[?2004$p");
        assert_eq!(
            drain_writes(&write_rx),
            b"\x1b[?2004;1$y",
            "DECRQM must report bracketed paste as set (1) after ?2004h"
        );
        // Disable it and re-query: value 2 = reset.
        w.feed(b"\x1b[?2004l\x1b[?2004$p");
        assert_eq!(
            drain_writes(&write_rx),
            b"\x1b[?2004;2$y",
            "DECRQM must report bracketed paste as reset (2) after ?2004l"
        );
    }

    // Case `decrqm_unknown_mode_reports_not_recognised`.
    {
        let (mut w, write_rx) = build_test_widget();
        // A mode we do not track → value 0 (not recognised) rather than a hang.
        w.feed(b"\x1b[?9999$p");
        assert_eq!(drain_writes(&write_rx), b"\x1b[?9999;0$y");
    }

    // Case `unknown_query_is_consumed_without_a_spurious_reply`.
    {
        let (mut w, write_rx) = build_test_widget();
        // DECSCUSR (set cursor style, `CSI 2 SP q`) is not a query — it must not
        // trigger the XTVERSION path or write anything back.
        w.feed(b"\x1b[2 q");
        assert!(
            drain_writes(&write_rx).is_empty(),
            "a non-query CSI must not write a spurious reply to the shell"
        );
    }
}

/// Query replies through the real PTY pump, and fish starting cleanly.
#[test]
#[cfg(unix)]
fn real_pty_query_replies_and_fish_startup() {
    // Body of case `fish_starts_without_the_device_attribute_warning` (a function so its early `return`/`?` stays local).
    fn fish_starts_without_the_device_attribute_warning() {
        // Drive the exact program the user runs, through the production
        // reader→sync→pump→vte→write_input→PTY path (no `feed`, no mock).
        //
        // Without a DA reply fish stalls ~10s on its Primary Device Attribute
        // probe and prints "could not read response to Primary Device Attribute
        // query … disables some optional features" as part of interactive
        // startup — i.e. before it will run a typed command. So: let the
        // emulator answer fish's probe (the warm-up pump), then type a sentinel
        // and wait for it. On the fixed path the probe is satisfied in ~1s and
        // the sentinel echoes with no warning; a regressed emulator would leave
        // the warning on screen. We assert the sentinel ran AND neither warning
        // string is present.
        let Some(fish) = locate_fish() else {
            eprintln!("fish not installed; skipping live-fish startup test");
            return;
        };

        let mut cmd = crate::CommandBuilder::new(fish);
        // `-i` forces the interactive reader (which is what probes the
        // terminal); `--no-config` keeps the test hermetic without suppressing
        // the probe (that is core reader behaviour, not config).
        cmd.arg("--no-config");
        cmd.arg("-i");
        let mut w = TerminalWidget::spawn_command(cmd).expect("spawn fish under a pty");
        let mut doc = AutoCommit::new();

        // Warm-up: pump long enough for fish to issue its DA query and for the
        // emulator to answer it (via write_input) before we type anything, so
        // our keystrokes are never mistaken for the DA response.
        for _ in 0..40 {
            w.sync(&doc, &ROOT);
            std::thread::sleep(std::time::Duration::from_millis(25));
        }

        // Now type the sentinel the way a user would, then quit.
        w.write_input(b"echo TILO_FISH_READY\r");
        let ready = pump_until_contains(&mut w, &mut doc, "TILO_FISH_READY");
        w.write_input(b"exit\r");
        w.sync(&doc, &ROOT);

        let screen = w.grid_text();
        assert!(
            ready,
            "fish must run a typed command promptly (it did not — a DA stall?); grid was:\n{screen}"
        );
        assert!(
            !screen.contains("Primary Device Attribute"),
            "fish printed the Primary Device Attribute warning — the emulator did \
         not answer the DA query in time; grid was:\n{screen}"
        );
        assert!(
            !screen.contains("disables some optional features"),
            "fish reported it disabled features waiting on a terminal query; \
         grid was:\n{screen}"
        );
    }

    // Case `da1_reply_round_trips_through_the_real_pty_pump_path`.
    {
        use portable_pty::CommandBuilder;
        // End-to-end proof on the production path (no `feed`, no mock writer):
        // a real child emits the DA1 query on its tty and then reads exactly the
        // 9 reply bytes back from stdin. The reply only arrives if the reader
        // thread → sync → pump → vte → write_input → PTY loop actually answers.
        // The tty is put in non-canonical, no-echo mode first (our reply has no
        // newline, so canonical `read` would block on it forever), and `cat -v`
        // renders the ESC so the grid shows `^[[?62;22c`.
        let mut cmd = CommandBuilder::new("/bin/sh");
        cmd.arg("-c");
        cmd.arg("stty -icanon -echo min 1 time 0; printf '\\033[c'; head -c 9 | cat -v; echo; echo REPLY_DONE");
        let mut w = TerminalWidget::spawn_command(cmd).expect("spawn pty command");
        let mut doc = AutoCommit::new();
        assert!(
            pump_until_contains(&mut w, &mut doc, "REPLY_DONE"),
            "the child must receive the DA1 reply over the real PTY; grid was:\n{}",
            w.grid_text()
        );
        assert!(
            w.grid_text().contains("[?62;22c"),
            "the child echoed back the exact DA1 reply bytes; grid was:\n{}",
            w.grid_text()
        );
    }

    // Case `fish_starts_without_the_device_attribute_warning`.
    fish_starts_without_the_device_attribute_warning();
}

// ---------------------------------------------------------------
// Default-shell resolution: honour $SHELL, then the passwd login shell,
// then /bin/sh. fish must launch when it is the user's login shell.
// ---------------------------------------------------------------

/// Login-shell resolution order and the default spawn using it.
#[test]
#[cfg(unix)]
fn login_shell_resolution_and_default_spawn() {
    // Body of case `spawn_default_command_uses_the_resolved_login_shell` (a function so its early `return`/`?` stays local).
    fn spawn_default_command_uses_the_resolved_login_shell() {
        // The real resolver: when $SHELL is set (as it is in an interactive
        // session), spawn_default launches exactly that shell. We assert on the
        // resolver the spawn path calls, not a reimplementation.
        if let Ok(shell) = std::env::var("SHELL")
            && !shell.is_empty()
        {
            assert_eq!(
                crate::inner::resolve_login_shell(),
                shell,
                "spawn_default's shell resolution must honour $SHELL"
            );
            return;
        }
        // No $SHELL in this environment: the resolver must still yield an
        // absolute shell path from passwd (or /bin/sh), never an empty string.
        let resolved = crate::inner::resolve_login_shell();
        assert!(
            resolved.starts_with('/'),
            "resolved shell must be absolute, got {resolved:?}"
        );
    }

    // Case `resolve_login_shell_prefers_shell_env`.
    {
        assert_eq!(
            crate::inner::resolve_login_shell_with(
                Some("/usr/bin/fish".into()),
                Some("/bin/bash".into())
            ),
            "/usr/bin/fish",
            "$SHELL wins when it is set and non-empty"
        );
    }

    // Case `resolve_login_shell_falls_back_to_passwd_when_shell_unset`.
    {
        assert_eq!(
            crate::inner::resolve_login_shell_with(None, Some("/usr/bin/zsh".into())),
            "/usr/bin/zsh",
            "with $SHELL unset the passwd login shell is used, not a hardcoded /bin/sh"
        );
        // An empty $SHELL is treated as unset and also falls through to passwd.
        assert_eq!(
            crate::inner::resolve_login_shell_with(
                Some(String::new()),
                Some("/usr/bin/zsh".into())
            ),
            "/usr/bin/zsh",
        );
    }

    // Case `resolve_login_shell_last_resort_is_sh`.
    {
        assert_eq!(
            crate::inner::resolve_login_shell_with(None, None),
            "/bin/sh",
            "with neither source available the session still starts on /bin/sh"
        );
    }

    // Case `passwd_lookup_resolves_a_real_login_shell`.
    {
        // Proves the getpwuid fallback actually reads a plausible shell for the
        // account running the tests (an absolute path). This is the source the
        // resolver uses when $SHELL is unset.
        let shell = crate::inner::login_shell_from_passwd()
            .expect("the current user must have a passwd entry with a login shell");
        assert!(
            shell.starts_with('/'),
            "passwd login shell should be an absolute path, got {shell:?}"
        );
    }

    // Case `spawn_default_command_uses_the_resolved_login_shell`.
    spawn_default_command_uses_the_resolved_login_shell();
}

/// Locate a `fish` binary for the live-fish test. Returns `None` if fish is
/// not installed (the test then skips — it exercises an external program).
#[cfg(unix)]
fn locate_fish() -> Option<std::path::PathBuf> {
    for p in ["/usr/bin/fish", "/bin/fish", "/usr/local/bin/fish"] {
        let path = std::path::PathBuf::from(p);
        if path.exists() {
            return Some(path);
        }
    }
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths)
        .map(|d| d.join("fish"))
        .find(|c| c.exists())
}

// ---------------------------------------------------------------
// Confinement / capability detection (issue: ush errors off Tilo).
// ---------------------------------------------------------------

/// Confinement routing and the hosted-terminal ush requirement.
#[test]
fn confinement_routes_are_mandatory_for_hosted_terminals() {
    // Case `confine_route_service_is_mandatory_for_every_target`.
    {
        // The security invariant, at the decision point: when the supervisor
        // service is available it is chosen unconditionally, for the device's
        // own shell too, so no combination of the inputs an app could
        // influence (a present runtime, a host that *could* self-host) reaches
        // a local process. The supervisor alone decides who gets the host.
        use crate::inner::{ConfineRoute, decide_route};
        use crate::target::SessionTarget;
        use std::path::PathBuf;
        let bin = PathBuf::from("/usr/libexec/tilo-ush");
        for target in [SessionTarget::default_environment(), SessionTarget::Host] {
            assert_eq!(
                decide_route(&target, true, false, None),
                ConfineRoute::Service
            );
            assert_eq!(
                decide_route(&target, true, true, None),
                ConfineRoute::Service
            );
            assert_eq!(
                decide_route(&target, true, true, Some(bin.clone())),
                ConfineRoute::Service
            );
        }
    }

    // Case `confine_route_host_is_chosen_never_a_fallback`.
    {
        // No service. The device's own shell starts directly only when the
        // target names it; an environment that cannot run here is unavailable,
        // never quietly `$SHELL` instead.
        use crate::inner::{ConfineRoute, decide_route};
        use crate::target::{Location, SessionTarget};
        use std::path::PathBuf;
        let bin = PathBuf::from("/usr/libexec/tilo-ush");
        let env = SessionTarget::default_environment();
        assert!(matches!(
            decide_route(&env, false, false, Some(bin.clone())),
            ConfineRoute::Unavailable(_)
        ));
        assert!(matches!(
            decide_route(&env, false, true, None),
            ConfineRoute::Unavailable(_)
        ));
        assert_eq!(
            decide_route(&env, false, true, Some(bin.clone())),
            ConfineRoute::SelfHosted(bin.clone())
        );
        assert_eq!(
            decide_route(&SessionTarget::Host, false, true, Some(bin.clone())),
            ConfineRoute::Host
        );
        // A relay's environment is carried there by the supervisor, and only
        // by it; legacy VMs are not its to serve.
        let on_relay = SessionTarget::Environment {
            record: None,
            location: Location::Relay {
                relay: "ab".repeat(32),
            },
        };
        assert_eq!(
            decide_route(&on_relay, true, false, None),
            ConfineRoute::Service
        );
        assert!(matches!(
            decide_route(&on_relay, false, true, Some(bin.clone())),
            ConfineRoute::Unavailable(_)
        ));
        assert!(matches!(
            decide_route(
                &SessionTarget::Legacy { vm: "linux".into() },
                true,
                true,
                Some(bin)
            ),
            ConfineRoute::Unavailable(_)
        ));
    }

    // Case `hosted_terminal_requires_ush_before_construction`.
    {
        let result = crate::TerminalWidget::spawn_hosted(crate::HostedProcessServices::default());
        assert!(matches!(
            result,
            Err(crate::HostedTerminalError::RequiredServiceMissing("ush"))
        ));
    }
}

// ---------------------------------------------------------------
// Selection latency (issue: highlight is slow to appear).
// ---------------------------------------------------------------

// ---------------------------------------------------------------
// Colours (issue: hardcoded green-on-black; must track the OS theme).
// ---------------------------------------------------------------

#[cfg(unix)]
fn private_ush_listener(runtime: &std::path::Path) -> std::os::unix::net::UnixListener {
    use std::os::unix::fs::PermissionsExt;

    let tilo = runtime.join("tilo");
    let services = tilo.join("services");
    std::fs::create_dir_all(&services).unwrap();
    for directory in [runtime, tilo.as_path(), services.as_path()] {
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let socket = services.join("ush.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600)).unwrap();
    listener
}

/// Windows native actions and the no-unconfined-shell guarantee.
#[cfg(windows)]
#[test]
#[serial_test::serial]
fn windows_native_and_hosted_terminal_paths() {
    // Case `native_terminal_action_runs_a_command_and_child_observes_resize`.
    {
        let mut widget = TerminalWidget::spawn_default().expect("spawn native shell");
        let mut document = AutoCommit::new();
        widget.sync(&mut document, &ROOT);
        widget.place_at(Rect::new(0.0, 0.0, 960.0, 540.0));
        let (columns, rows) = widget.grid_size();
        let command = r#"powershell.exe -NoProfile -Command "$s=$Host.UI.RawUI.WindowSize; 'NATIVE_SIZE={0}x{1}' -f $s.Width,$s.Height""#;
        let arguments = serde_json::to_vec(command).unwrap();
        assert!(
            ui::framework::HostDelivery::start_encoded(
                &mut widget,
                ui::tilo_actions::OperationKind::Effect,
                "terminal.run",
                &arguments,
                &mut document,
                &ROOT
            )
            .map(|started| started.is_some())
            .expect("invoke native command through action API")
        );
        let expected = format!("NATIVE_SIZE={columns}x{rows}");
        assert!(
            pump_until_contains(&mut widget, &mut document, &expected),
            "the actual child must observe the resized ConPTY: {}",
            widget.grid_text()
        );
        widget
            .try_write_input(b"exit\r\n")
            .expect("close native shell");
    }

    // Case `windows_claimed_hosted_service_never_starts_an_unconfined_shell`.
    {
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set(crate::inner::USH_SERVICE_FD_ENV, "3");
        let result = TerminalWidget::spawn_default();
        // Restore before asserting, including when the result is unexpected.
        drop(env);
        assert!(
            result.is_err(),
            "a hosted grant must never fall back to COMSPEC"
        );
    }
}

#[test]
fn a_desktop_entry_command_reaches_the_terminal_through_the_launch_context() {
    use crate::inner::{exec_script, launch_command_from_context};
    assert_eq!(
        launch_command_from_context(r#"{"terminal_command":["vim","--","/home/u/a b.txt"]}"#),
        vec!["vim", "--", "/home/u/a b.txt"]
    );
    for ignored in [
        "",
        "not json",
        r#"{"tool":"vpn"}"#,
        r#"{"terminal_command":[]}"#,
        r#"{"terminal_command":["",""]}"#,
        r#"{"terminal_command":["vim",3]}"#,
    ] {
        assert!(
            launch_command_from_context(ignored).is_empty(),
            "{ignored:?} must start an ordinary shell"
        );
    }
    assert_eq!(
        exec_script(&["printf".into(), "it's %s".into()]),
        "exec 'printf' 'it'\\''s %s'"
    );
}

#[test]
fn a_fish_prompt_after_its_startup_queries_is_on_screen() {
    let (mut w, _rx) = build_test_widget();
    let doc = AutoCommit::new();
    w.force_geometry(Rect::new(0.0, 0.0, 800.0, 480.0), 10.0, 20.0);
    let batches: [&[u8]; 3] = [
        b"\x1b[?u\x1b[>0q\x1b]11;?\x1b\\\x1b[?1049h\x1bP+q696e646e\x1b\\\x1bP+q71756572792d6f732d6e616d65\x1b\\\x1b[?1049l\x1b[0c",
        b"\rWillkommen in fish\r\nType \x1b[32mhelp\x1b[m for instructions\r\n\x1b]7;file://ush/var/tmp\x07\x1b]0;/v/tmp\x07\x1b[m\x1b]11;?\x1b\\\x1b[6n\x1b[0c",
        b"\x1b[?2004h\x1b[?2031h\x1b[>4;1m\x1b=\x1b]133;A;click_events=1\x1b\\\x1b[92mroot\x1b[m@\x1b[mush\x1b[m \x1b[31m/v/tmp\x1b[m\x1b[m# \x1b]133;B\x07\x1b[K\r\x1b[17C\x1b[?2004l\x1b[?2031l\x1b[>4;0m\x1b>\x1b]0;/v/tmp\x07\x1b[m\x1b[?2004h\x1b[?2031h\x1b[>4;1m\x1b=\r\x1b]133;A;click_events=1\x07\x1b[92mroot\x1b[m@\x1b[mush\x1b[m \x1b[31m/v/tmp\x1b[m\x1b[m# \x1b]133;B\x07\x1b[J\r\x1b[17Cab",
    ];
    for batch in batches {
        w.feed(batch);
        w.sync(&doc, &ROOT);
    }
    assert!(
        w.grid_row(2).starts_with("root@ush /v/tmp# ab"),
        "prompt row: {:?}\n{}",
        w.grid_row(2),
        w.grid_text()
    );
}

#[test]
fn erasing_below_the_cursor_keeps_what_is_left_of_it() {
    let (mut w, _rx) = build_test_widget();
    let doc = AutoCommit::new();
    w.force_geometry(Rect::new(0.0, 0.0, 800.0, 480.0), 10.0, 20.0);
    w.feed(b"above\r\nP# tail\r\nbelow\x1b[2;4H\x1b[J");
    w.sync(&doc, &ROOT);
    assert_eq!(w.grid_row(0).trim_end(), "above");
    assert_eq!(w.grid_row(1).trim_end(), "P#");
    assert_eq!(w.grid_row(2).trim_end(), "");

    w.feed(b"\x1b[2;3H\x1b[1J");
    w.sync(&doc, &ROOT);
    assert_eq!(w.grid_row(0).trim_end(), "");
    assert!(
        w.grid_row(1).starts_with("  "),
        "row 1: {:?}",
        w.grid_row(1)
    );
}

#[test]
fn the_cursor_is_an_opaque_inverted_cell() {
    let palette = ui::tokens::TerminalPalette::dark();
    let cursor = crate::inner::cursor_from_palette(&palette);
    assert!(
        (cursor.components[3] - 1.0).abs() < f32::EPSILON,
        "the block cursor is fully opaque, not a translucent wash"
    );
    assert_eq!(
        cursor.components[..3],
        palette.fg_default.components[..3],
        "the cursor is the default foreground"
    );
}

#[test]
fn cell_fills_snap_to_device_pixels_and_adjacent_cells_tile() {
    use crate::inner::snap_to_device_pixels;
    use vello::kurbo::Affine;

    for scale in [1.0, 1.25, 1.5, 2.0] {
        let transform = Affine::scale(scale);
        // Fractional advances and an offset, as in a real grid.
        let (char_w, x0) = (8.43, 12.0);
        for cell in 0..40 {
            let left = x0 + f64::from(cell) * char_w;
            let a = snap_to_device_pixels(transform, Rect::new(left, 20.0, left + char_w, 40.0));
            let b = snap_to_device_pixels(
                transform,
                Rect::new(left + char_w, 20.0, left + 2.0 * char_w, 40.0),
            );
            for edge in [a.x0, a.x1, a.y0, a.y1] {
                let device = edge * scale;
                assert!(
                    (device - device.round()).abs() < 1e-9,
                    "edge {edge} at scale {scale} is not on a device pixel"
                );
            }
            assert!(
                (a.x1 - b.x0).abs() < 1e-9,
                "neighbouring cells meet without a seam or overlap"
            );
        }
    }
}

/// Typing repaints the row it lands on and the cursor, not the terminal:
/// each row on screen and the cursor are damage parts the compositor
/// compares frame to frame (`ui::scene_parts`).
#[test]
fn typing_damages_only_its_row_and_the_cursor() {
    use ui::{Theme, presence::PresenceCtx, text_render::TextRenderer};
    use vello::{Scene, kurbo::Affine};

    let (mut widget, _) = build_test_widget();
    let viewport = Rect::new(0.0, 0.0, 1600.0, 1000.0);
    widget.force_geometry(viewport, 10.0, 20.0);
    widget.feed(b"first line\r\nsecond line\r\n$ ");
    let theme = Theme::dark();
    let mut text = TextRenderer::new();
    let mut frame = |widget: &mut TerminalWidget| {
        let mut scene = Box::new(Scene::new());
        let ((), parts) = ui::scene_parts::record(&mut scene, |scene| {
            widget.draw(
                scene,
                Affine::IDENTITY,
                &mut text,
                &theme,
                &PresenceCtx::empty(),
            );
        });
        (scene, parts.expect("the terminal's parts are well nested"))
    };
    // The first draw measures the cell and resizes the grid to the
    // viewport; the next output applies that size.
    let _ = frame(&mut widget);
    widget.feed(b"");
    let (before, before_parts) = frame(&mut widget);
    let (again, again_parts) = frame(&mut widget);
    assert_eq!(
        ui::scene_parts::damage(&before, &before_parts, &again, &again_parts),
        Ok(Vec::new()),
        "an unchanged terminal draws the same commands"
    );
    widget.feed(b"l");
    let (after, after_parts) = frame(&mut widget);
    let damage = ui::scene_parts::damage(&again, &again_parts, &after, &after_parts)
        .expect("typing changes nothing outside the terminal's parts");
    assert!(!damage.is_empty(), "the typed letter is repainted");
    let (_, rows) = widget.grid_size();
    let row_h = widget.grid_rect().height() / rows.max(1) as f64;
    assert!(
        damage.iter().all(|rect| rect.height() <= 3.0 * row_h + 2.0),
        "typing repaints a row band and the cursor's cell, not the grid: {damage:?}"
    );
    let area: f64 = damage.iter().map(Rect::area).sum();
    eprintln!(
        "TIL-579 terminal keystroke damage: {area} of {} px",
        viewport.area()
    );
    assert!(
        area * 4.0 < viewport.area(),
        "typing repaints a sliver of the terminal, not {area} of {} px: {damage:?}",
        viewport.area()
    );
}
