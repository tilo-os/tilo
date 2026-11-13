use crate::{Attr, GridPos, Selection, TerminalWidget};
use automerge::{AutoCommit, ROOT};
use portable_pty::{MasterPty, PtySize};
use std::io::{Read, Write};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use tilo_test_env::hang_bound;
use ui::State;
use ui::framework::ResourceSurface;
use ui::tilo_resource::Resource as _;
use ui::widget::DrawChild;
use ui::widget::Widget as _;
use vello::kurbo::Rect;

#[path = "chrome_tests.rs"]
mod chrome;

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

#[test]
fn pty_output_is_read_and_pumped_into_redraws_and_watermark() {
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
            if cfg!(target_vendor = "apple") {
                PtyReadDisposition::Retry
            } else {
                PtyReadDisposition::Closed
            },
            "macOS reports a master without a slave as the end of the file, Linux as EIO"
        );
        assert_eq!(
            classify_pty_read(&Ok(0), || false),
            PtyReadDisposition::Closed,
            "once the session's lifetime socket closes, the end of the file is the guest's exit"
        );
        assert_eq!(
            classify_pty_read(&Err(std::io::Error::from_raw_os_error(libc::EBADF)), || {
                true
            }),
            PtyReadDisposition::Closed
        );
    }

    {
        let (mut widget, _writes) = build_test_widget();
        assert!(!widget.has_received_pty_output());

        widget.feed(b"\x1b[32m\x1b[?25h");

        assert!(
            widget.has_received_pty_output(),
            "control-only startup output still proves the PTY is live"
        );
    }

    {
        let (mut widget, _write_rx, output_tx) = build_test_widget_io();
        let state = AutoCommit::new();
        assert!(!widget.has_received_pty_output());

        output_tx.send(vec![b'\x1b', b'[', b'0', b'm']).unwrap();
        assert!(widget.update(&state, &ROOT));
        assert!(widget.has_received_pty_output());
    }
}

#[cfg(unix)]
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "cases merged from separate tests; each case stays self-contained"
)]
#[serial_test::serial]
fn hosted_service_sockets_are_probed_and_never_fall_back() {
    {
        use std::io::BufRead as _;

        let open_with = |bound: Option<std::time::Duration>, delay: std::time::Duration| {
            let (terminal_end, supervisor_end) =
                std::os::unix::net::UnixStream::pair().expect("socket pair");
            terminal_end.set_read_timeout(bound).unwrap();
            let supervisor = std::thread::spawn(move || {
                let mut reader = std::io::BufReader::new(&supervisor_end);
                let mut hello = String::new();
                reader.read_line(&mut hello).unwrap();
                assert!(hello.contains("\"op\":\"hello\""), "{hello}");
                (&supervisor_end)
                    .write_all(b"{\"result\":\"ready\",\"protocol\":2}\n")
                    .unwrap();
                let mut request = String::new();
                reader.read_line(&mut request).unwrap();
                std::thread::sleep(delay);
                (&supervisor_end)
                    .write_all(b"{\"result\":\"no-runtime\",\"reason\":\"checked\"}\n")
                    .unwrap();
                request
            });
            let opened = crate::inner::ush_backend_on(
                &terminal_end,
                &[],
                &crate::target::SessionTarget::default_environment(),
                crate::Requester::Person,
                None,
                None,
            );
            assert!(supervisor.join().unwrap().contains("open-session"));
            (opened.err(), terminal_end.read_timeout().unwrap())
        };
        let answered = |error: &Option<crate::OpenError>| matches!(error, Some(crate::OpenError::NoRuntime(reason)) if reason == "checked");

        let connect_bound = std::time::Duration::from_millis(250);
        let (error, _) = open_with(Some(connect_bound), std::time::Duration::from_millis(700));
        assert!(answered(&error), "the late answer was not read: {error:?}");

        let (error, after) = open_with(None, std::time::Duration::ZERO);
        assert!(answered(&error), "{error:?}");
        assert_eq!(after, None);
        let long = std::time::Duration::from_secs(60);
        let (error, after) = open_with(Some(long), std::time::Duration::ZERO);
        assert!(answered(&error), "{error:?}");
        assert_eq!(after, Some(long));
    }

    {
        use std::io::BufRead as _;

        const REPLY: &[u8] = b"{\"result\":\"session\",\"session\":\"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\",\"environment\":\"default\",\"runs\":true,\"protocol\":2}\n";
        let (terminal_end, supervisor_end) =
            std::os::unix::net::UnixStream::pair().expect("socket pair");
        let supervisor = std::thread::spawn(move || {
            let mut reader = std::io::BufReader::new(&supervisor_end);
            let mut hello = String::new();
            reader.read_line(&mut hello).unwrap();
            assert!(hello.contains("\"op\":\"hello\""), "{hello}");
            (&supervisor_end)
                .write_all(b"{\"result\":\"ready\",\"protocol\":2}\n")
                .unwrap();
            let mut request = String::new();
            reader.read_line(&mut request).unwrap();
            let pair = portable_pty::native_pty_system()
                .openpty(PtySize::default())
                .unwrap();
            let (lease, terminal_lease) =
                std::os::unix::net::UnixStream::pair().expect("lease pair");
            runtime_portal::fd_transport::send_with_fds(
                &supervisor_end,
                REPLY,
                // SAFETY: `pair` owns the master and outlives this call.
                &[
                    unsafe {
                        std::os::fd::BorrowedFd::borrow_raw(pair.master.as_raw_fd().unwrap())
                    },
                    std::os::fd::AsFd::as_fd(&terminal_lease),
                ],
            )
            .unwrap();
            (request, supervisor_end, pair, lease)
        });
        let opened = crate::inner::ush_backend_on(
            &terminal_end,
            &[],
            &crate::target::SessionTarget::default_environment(),
            crate::Requester::Person,
            None,
            None,
        );
        let (request, _supervisor_end, _pair, _lease) = supervisor.join().unwrap();
        let request: serde_json::Value = serde_json::from_str(&request).unwrap();
        let backend = match opened {
            Ok(backend) => backend,
            Err(error) => panic!("a session reply that marks runs must open: {error:?}"),
        };
        assert_eq!(backend.shell_name, "ush");
        assert_eq!(
            backend.runs.as_deref(),
            request["runs"].as_str(),
            "the session marks its runs with the key the request named"
        );
        assert!(backend.runs.is_some());
    }

    {
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

    {
        use std::io::{BufRead, BufReader};

        let runtime = tempfile::tempdir().unwrap();
        let listener = private_ush_listener(runtime.path());
        let server = std::thread::spawn(move || {
            use std::io::Write;
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut hello = String::new();
            reader.read_line(&mut hello).unwrap();
            assert!(hello.contains("\"op\":\"hello\""), "{hello}");
            stream
                .write_all(b"{\"result\":\"ready\",\"protocol\":2}\n")
                .unwrap();
            let mut request = String::new();
            reader.read_line(&mut request).unwrap();
            assert!(request.contains("\"op\":\"open-session\""), "{request}");
            assert!(
                request.contains("\"target\":{\"kind\":\"environment\"}"),
                "{request}"
            );
            let pair = portable_pty::native_pty_system()
                .openpty(portable_pty::PtySize::default())
                .unwrap();
            let (_supervisor_lease, terminal_lease) =
                std::os::unix::net::UnixStream::pair().unwrap();
            runtime_portal::fd_transport::send_with_fds(
                &stream,
                b"{\"result\":\"session\",\"protocol\":2,\"session\":\"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\",\"environment\":\"default\"}\n",
                // SAFETY: `pair` owns the master and outlives this call.
                &[
                    unsafe { std::os::fd::BorrowedFd::borrow_raw(pair.master.as_raw_fd().unwrap()) },
                    std::os::fd::AsFd::as_fd(&terminal_lease),
                ],
            )
            .unwrap();
        });

        let services = crate::HostedProcessServices::discover_from_runtime_dir(runtime.path());
        let mut widget = crate::TerminalWidget::spawn_hosted(services)
            .expect("the discovered ush connection must use the supervisor protocol");
        let doc = session_record_at(&crate::target::SessionTarget::default_environment());
        ui::widget::Widget::sync(&mut widget, &doc, &automerge::ROOT);
        tilo_test_env::wait_until("the healthy open to finish", || {
            ui::widget::Widget::update(&mut widget, &doc, &automerge::ROOT);
            !matches!(widget.session_state(), crate::sessions::OpenState::Opening)
        });
        assert_eq!(widget.session_state(), &crate::sessions::OpenState::Open);
        drop(widget);
        server.join().unwrap();
    }

    {
        let runtime = tempfile::tempdir().unwrap();
        let services = crate::HostedProcessServices::discover_from_runtime_dir(runtime.path());
        assert!(services.ush.is_none());
        assert!(matches!(
            crate::TerminalWidget::spawn_hosted(services),
            Err(crate::HostedTerminalError::RequiredServiceMissing("ush"))
        ));
    }

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

    {
        use std::os::unix::net::UnixStream;
        let (client, server) = UnixStream::pair().unwrap();
        drop(server);
        let mut widget =
            crate::TerminalWidget::spawn_hosted(crate::HostedProcessServices { ush: Some(client) })
                .unwrap();
        let doc = session_record_at(&crate::target::SessionTarget::default_environment());
        ui::widget::Widget::sync(&mut widget, &doc, &automerge::ROOT);
        tilo_test_env::wait_until("the failed open to finish", || {
            ui::widget::Widget::update(&mut widget, &doc, &automerge::ROOT);
            !matches!(widget.session_state(), crate::sessions::OpenState::Opening)
        });
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

fn session_record_at(target: &crate::target::SessionTarget) -> AutoCommit {
    let mut record = ui::tilo_resource::Replica::<crate::resource::Terminal>::new();
    record
        .apply(crate::resource::Terminal::RECORD_TARGET.with((target.encode(), None::<String>)))
        .expect("the target is recorded");
    record.doc().clone()
}

pub(crate) fn drain_writes(rx: &std::sync::mpsc::Receiver<Vec<u8>>) -> Vec<u8> {
    let mut out = Vec::new();
    while let Ok(b) = rx.try_recv() {
        out.extend_from_slice(&b);
    }
    out
}

fn send_left_press(widget: &mut TerminalWidget, doc: &mut AutoCommit) {
    let ev = ui::synthetic_input::mouse_press();
    ui::testing::Dispatch::new().doc(doc).send(widget, &ev);
}

fn send_left_release(widget: &mut TerminalWidget, doc: &mut AutoCommit) {
    let ev = ui::synthetic_input::mouse_release();
    ui::testing::Dispatch::new().doc(doc).send(widget, &ev);
}

fn send_cursor_move(widget: &mut TerminalWidget, doc: &mut AutoCommit, x: f64, y: f64) {
    let ev = ui::synthetic_input::cursor_moved(x, y);
    ui::testing::Dispatch::new().doc(doc).send(widget, &ev);
}

fn send_mouse_button_mods(
    widget: &mut TerminalWidget,
    doc: &mut AutoCommit,
    button: winit::event::MouseButton,
    state: winit::event::ElementState,
    shift: bool,
) {
    use winit::keyboard::ModifiersState;
    let modifiers = if shift {
        ModifiersState::SHIFT
    } else {
        ModifiersState::empty()
    };
    let ev = ui::synthetic_input::mouse_input(state, button);
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

pub(crate) fn send_key_mods(
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
    use winit::event::ElementState;

    let mut ev = ui::synthetic_input::key_event(key, None, ElementState::Pressed);
    ev.text = match &ev.logical_key {
        winit::keyboard::Key::Character(s) => Some(s.clone()),
        _ => None,
    };

    let win_ev = ui::synthetic_input::keyboard_input(ev);
    ui::testing::Dispatch::new()
        .modifiers(state)
        .doc(doc)
        .send(widget, &win_ev);
}

pub(crate) fn send_key_without_text(
    widget: &mut TerminalWidget,
    doc: &mut AutoCommit,
    key: winit::keyboard::Key,
) -> bool {
    use winit::event::ElementState;

    let ev = ui::synthetic_input::key_event(key, None, ElementState::Pressed);
    ui::testing::Dispatch::new()
        .doc(doc)
        .send(widget, &ui::synthetic_input::keyboard_input(ev))
        .handled
}

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

#[test]
fn control_space_reaches_the_native_editor_as_nul() {
    use winit::event::ElementState;
    use winit::keyboard::{Key, KeyCode, ModifiersState, NamedKey, PhysicalKey, SmolStr};

    let (mut widget, writes) = build_test_widget();
    let mut doc = AutoCommit::new();
    widget.force_geometry(Rect::new(0.0, 0.0, 800.0, 480.0), 10.0, 20.0);

    for alt in [false, true] {
        let modifiers = if alt {
            ModifiersState::CONTROL | ModifiersState::ALT
        } else {
            ModifiersState::CONTROL
        };
        for key in [
            Key::Named(NamedKey::Space),
            Key::Character(SmolStr::new_static(" ")),
            Key::Unidentified(winit::keyboard::NativeKey::Unidentified),
        ] {
            let mut event = ui::synthetic_input::key_event(key, None, ElementState::Pressed);
            event.physical_key = PhysicalKey::Code(KeyCode::Space);
            assert!(
                ui::testing::Dispatch::new()
                    .modifiers(modifiers)
                    .doc(&mut doc)
                    .send(&mut widget, &ui::synthetic_input::keyboard_input(event),)
                    .handled,
                "the terminal must consume modified Space"
            );
            assert_eq!(
                drain_writes(&writes),
                if alt {
                    b"\x1b\0".as_slice()
                } else {
                    b"\0".as_slice()
                },
                "all native key spellings must reach Readline's mark command"
            );
        }
    }
}

#[test]
fn terminal_grid_initialises_prints_and_is_a_library_widget() {
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

    {
        let (mut widget, _rx) = build_test_widget();

        widget.feed(b"");
        assert_eq!(widget.grid_row(0).len(), 0, "blank lines are seeded empty");

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

    {
        fn _takes_widget<T: ui::widget::Widget>(_: T) {}
        #[allow(
            unused_imports,
            reason = "the import itself is the compile-time public type assertion"
        )]
        use crate::TerminalWidget as _;
    }

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

#[test]
fn terminal_draw_and_disconnected_state_follow_theme_and_text_scale() {
    {
        use ui::widget::Widget;

        let mut widget = TerminalWidget::spawn_command(shell_command("true", "exit /b 0"))
            .expect("spawn short-lived pty command");
        let doc = AutoCommit::new();
        tilo_test_env::wait_until("PTY EOF to settle the session state", || {
            widget.sync(&doc, &ROOT);
            widget.is_disconnected()
        });
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

    {
        use ui::Theme;
        let theme = Theme::dark();
        let mono = theme.tokens.font_mono;
        assert!(
            !mono.family.is_empty(),
            "theme must expose mono font tokens"
        );
    }

    {
        use ui::{Theme, presence::PresenceCtx, text_render::TextRenderer};
        use vello::{Scene, kurbo::Affine};

        let (mut widget, _) = build_test_widget();
        widget.force_geometry(Rect::new(0.0, 0.0, 390.0, 844.0), 4.0, 12.0);
        let (provisional_cols, _) = widget.grid_size();
        let theme = Theme::high_contrast().with_text_scale(2.0);
        let mut scene = Scene::new();
        let mut text_renderer = TextRenderer::new();
        widget.draw_child(
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

    {
        assert_eq!(TerminalWidget::disconnected_badge_label(), "Session ended");
    }
}

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
    let regex = FindOptions {
        mode: ui::widget::FindMode::Regex,
        ..FindOptions::default()
    };
    assert_eq!(Findable::find(&mut widget, "^c.t", regex).len(), 2);
    assert_eq!(
        Findable::find(
            &mut widget,
            "c.t",
            FindOptions {
                whole_word: true,
                ..regex
            }
        )
        .len(),
        3
    );
    assert!(
        Findable::find(&mut widget, "(", regex).is_empty(),
        "a pattern that does not compile finds nothing"
    );
    Findable::clear_find(&mut widget);
    assert!(widget.find_test_matches().is_empty());
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "cases merged from separate tests; each case stays self-contained"
)]
fn terminal_palette_tracks_the_theme() {
    {
        use ui::tokens::TerminalPalette;
        use vello::peniko::Color;

        let mut palette_a = TerminalPalette::dark();
        let mut palette_b = TerminalPalette::dark();

        palette_a.ansi[1] = Color::from_rgb8(10, 20, 30);
        palette_b.ansi[1] = Color::from_rgb8(250, 240, 230);
        palette_a.fg_default = Color::from_rgb8(11, 22, 33);
        palette_b.fg_default = Color::from_rgb8(240, 230, 220);
        palette_a.bg_default = Color::from_rgb8(1, 2, 3);
        palette_b.bg_default = Color::from_rgb8(250, 251, 252);

        let fg_a = crate::inner::map_ansi_256(1, &palette_a);
        let fg_b = crate::inner::map_ansi_256(1, &palette_b);
        assert_ne!(
            format!("{fg_a:?}"),
            format!("{:?}", fg_b),
            "palette swap must change ANSI 1 lookup"
        );

        let cursor_a = crate::inner::cursor_from_palette(&palette_a);
        let cursor_b = crate::inner::cursor_from_palette(&palette_b);
        assert_ne!(
            format!("{cursor_a:?}"),
            format!("{:?}", cursor_b),
            "palette swap must change cursor color"
        );

        let cube_a = crate::inner::map_ansi_256(42, &palette_a);
        let cube_b = crate::inner::map_ansi_256(42, &palette_b);
        assert_eq!(
            format!("{cube_a:?}"),
            format!("{:?}", cube_b),
            "6x6x6 ANSI cube is spec-defined and must not move with the theme"
        );
    }

    {
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

        let pre = widget.last_fg_default();
        assert_eq!(
            format!("{pre:?}"),
            format!("{:?}", Color::from_rgba8(0, 0, 0, 0)),
            "before first draw the cached fg must be the sentinel"
        );

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

    {
        use ui::ColorCompat;
        use ui::theme::Theme;
        let rgb = |c: vello::peniko::Color| (c.r(), c.g(), c.b());

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

#[test]
#[serial_test::serial]
fn selection_spans_anchor_drag_and_text_extraction() {
    {
        let (mut widget, _) = build_test_widget();
        let mut doc = AutoCommit::new();
        widget.force_geometry(Rect::new(0.0, 0.0, 800.0, 600.0), 10.0, 20.0);

        send_cursor_move(&mut widget, &mut doc, 100.0, 100.0);
        send_left_press(&mut widget, &mut doc);
        assert!(widget.selection().is_some());
        send_left_release(&mut widget, &mut doc);
        assert!(
            widget.selection().is_none(),
            "lone click must clear selection on release"
        );
        assert!(!widget.is_dragging());
    }

    {
        let (mut widget, _) = build_test_widget();
        let mut doc = AutoCommit::new();
        widget.force_geometry(Rect::new(0.0, 0.0, 800.0, 600.0), 10.0, 20.0);

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

        send_left_release(&mut widget, &mut doc);
        let sel2 = widget.selection().expect("selection survives release");
        assert_eq!(sel2.anchor, GridPos { row: 2, col: 5 });
        assert_eq!(sel2.cursor, GridPos { row: 6, col: 20 });
        assert!(!widget.is_dragging());
    }

    {
        let sel = Selection {
            anchor: GridPos { row: 3, col: 5 },
            cursor: GridPos { row: 3, col: 12 },
        };
        let spans = crate::inner::selection_row_spans(&sel, 80);
        assert_eq!(spans, vec![(3, 5, 12)]);
    }

    {
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

    {
        let sel = Selection {
            anchor: GridPos { row: 5, col: 7 },
            cursor: GridPos { row: 1, col: 2 },
        };
        let spans = crate::inner::selection_row_spans(&sel, 80);
        assert_eq!(spans.first(), Some(&(1, 2, 80)));
        assert_eq!(spans.last(), Some(&(5, 0, 7)));
    }

    {
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

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "cases merged from separate tests; each case stays self-contained"
)]
#[serial_test::serial]
fn clipboard_gestures_route_through_confirmed_actions() {
    {
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

    {
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

    {
        ui::clipboard_copy_primary("PASTED");
        let (mut w, write_rx) = build_test_widget();
        let mut doc = AutoCommit::new();
        w.force_geometry(Rect::new(0.0, 0.0, 800.0, 480.0), 10.0, 20.0);
        w.feed(b""); // init modes (no bracketed paste)
        send_cursor_move(&mut w, &mut doc, 100.0, 100.0);
        send_middle_press(&mut w, &mut doc);
        assert_eq!(drain_writes(&write_rx), b"PASTED");
    }

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

#[test]
#[serial_test::serial]
fn person_paste_is_immediate_while_published_paste_asks_for_confirmation() {
    use crate::resource::TerminalEffect;
    use vello::kurbo::Point;
    let mut app =
        ui::framework::testing::TestApp::<crate::resource::TerminalShell>::new().expect("terminal");
    let writes = app
        .with_surface(|terminal: &mut crate::TerminalView| {
            terminal.accept_input_for_test();
            terminal.attach_input_probe()
        })
        .expect("hosted terminal");
    app.settle();

    let (own, direct) = app
        .with_surface(|terminal: &mut crate::TerminalView| {
            let probe = TerminalEffect::Paste { text: "d".into() };
            (
                terminal.own_gesture_verdict(&probe),
                terminal.perform(probe),
            )
        })
        .expect("hosted terminal");
    assert_eq!((own, direct), (Some(true), Ok(())));
    let _ = drain_writes(&writes);

    ui::clipboard_copy("person shortcut paste");
    app.with_surface(|terminal: &mut crate::TerminalView| {
        use winit::keyboard::{Key, NamedKey, SmolStr};
        let mut doc = AutoCommit::new();
        terminal.set_focus(true);
        send_key(
            terminal,
            &mut doc,
            Key::Character(SmolStr::new_static("V")),
            true,
            true,
        );
        send_key(
            terminal,
            &mut doc,
            Key::Named(NamedKey::Insert),
            false,
            true,
        );
    })
    .expect("hosted terminal");
    app.settle();
    assert_eq!(
        drain_writes(&writes),
        b"person shortcut pasteperson shortcut paste",
        "the person's Ctrl+Shift+V and Shift+Insert paste with no confirmation step"
    );

    ui::clipboard_copy("person menu paste");
    app.with_surface(|terminal: &mut crate::TerminalView| {
        terminal.force_geometry(Rect::new(0.0, 0.0, 800.0, 480.0), 10.0, 20.0);
        terminal.open_context_menu_at(Point::new(20.0, 20.0));
        let paste = terminal
            .context_menu_labels()
            .iter()
            .position(|item| item == "Paste")
            .expect("menu has Paste");
        terminal.activate_menu_row_for_test(paste);
    })
    .expect("hosted terminal");
    app.settle();
    assert_eq!(drain_writes(&writes), b"person menu paste");

    let paste = TerminalEffect::Paste {
        text: "invoked paste".into(),
    };
    assert!(
        matches!(
            app.effect_by("agent:paste-test", &paste),
            Err(ui::tilo_actions::DoorError::ConfirmationRequired)
        ),
        "an agent's paste waits for confirmation"
    );
    assert!(
        matches!(
            app.effect(&paste),
            Err(ui::tilo_actions::DoorError::ConfirmationRequired)
        ),
        "a script's or the palette's paste waits for confirmation"
    );
    app.settle();
    assert!(
        drain_writes(&writes).is_empty(),
        "an unconfirmed paste never reaches the process"
    );
    app.effect_confirmed(&paste).expect("confirmed paste");
    app.settle();
    assert_eq!(drain_writes(&writes), b"invoked paste");
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "cases merged from separate tests; each case stays self-contained"
)]
fn scrollback_viewport_and_scrollbar_track_output() {
    {
        use vello::kurbo::Rect;
        let (mut w, _write_rx, out_tx) = build_test_widget_io();
        let doc = AutoCommit::new();
        w.force_geometry(Rect::new(0.0, 0.0, 800.0, 240.0), 10.0, 20.0);

        let mut payload = Vec::new();
        for i in 0..60 {
            payload.extend_from_slice(format!("LINE{i:03}\r\n").as_bytes());
        }
        w.feed(&payload);
        w.sync(&doc, &automerge::ROOT);

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

        out_tx.send(b"LINE060\r\n".to_vec()).unwrap();
        w.sync(&doc, &automerge::ROOT);
        assert_eq!(w.scroll_offset(), 0, "new output snaps to the live bottom");
        let after = w.visible_lines();
        assert!(
            after.iter().any(|l| l.contains("LINE060")),
            "bottom shows freshest: {after:?}"
        );
    }

    {
        use vello::kurbo::Rect;
        let (mut w, _rx) = build_test_widget();
        w.force_geometry(Rect::new(0.0, 0.0, 800.0, 240.0), 10.0, 20.0);
        w.force_grid(vec![" ".repeat(80); 20]);
        assert!(
            !w.scrollbar_visible(12),
            "20 blank rows behind a 12-row window is nothing to scroll to",
        );
    }

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

        send_key(
            &mut w,
            &mut doc,
            Key::Character(SmolStr::new_static("x")),
            false,
            false,
        );
        assert_eq!(w.scroll_offset(), 0, "keystroke snaps to bottom");
    }

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

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "cases merged from separate tests; each case stays self-contained"
)]
fn find_in_terminal_is_local_discoverable_and_reveals() {
    use crate::TerminalView;
    use ui::window_toolbar::{SearchOpening, take_search_request};

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
                let _ = take_search_request();
                send_key_with_modifiers(&mut view, &mut doc, Key::Character("f".into()), modifiers);
                assert_eq!(
                    drain_writes(&writes),
                    expected,
                    "{platform:?}: shell editing bytes"
                );
                assert_eq!(
                    take_search_request(),
                    None,
                    "{platform:?}: shell chord must not open Find"
                );
            }
        }
    }

    {
        use ui::{Platform, PlatformConventions};
        use winit::keyboard::{Key, ModifiersState};

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
            let _ = take_search_request();
            send_key_with_modifiers(&mut view, &mut doc, Key::Character("F".into()), modifiers);
            assert_eq!(
                take_search_request(),
                Some(SearchOpening::Find),
                "{platform:?}: the terminal's chord opens the window bar's search"
            );
            assert!(
                drain_writes(&writes).is_empty(),
                "Find must not type into the process"
            );
            send_key(&mut view, &mut doc, Key::Character("f".into()), true, false);
            assert_eq!(drain_writes(&writes), vec![0x06]);
        }
    }

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
        let find_label = crate::i18n::t("terminal-find-scrollback");
        assert_ne!(
            find_label, "terminal-find-scrollback",
            "toolkit supplies the visible label"
        );
        let find = menu
            .children
            .iter()
            .find(|item| item.name.as_deref() == Some(find_label.as_str()))
            .expect("Find entry");
        let _ = take_search_request();
        assert!(view.terminal_mut().perform_semantic_action(
            find.action_target.as_deref().expect("Find target"),
            &Action::Activate,
            &mut ui::widget::Edits::lend(&mut doc),
            &ROOT,
        ));
        view.sync(&doc, &ROOT);
        assert_eq!(take_search_request(), Some(SearchOpening::Find));
        assert!(
            drain_writes(&writes).is_empty(),
            "Find never writes shell commands"
        );
        assert_eq!(
            heads_before,
            doc.get_heads(),
            "Find remains private local view state"
        );
    }

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

    {
        use ui::widget::{InPlace, SearchDriver};

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
        assert_eq!(InPlace::of(&mut view), InPlace::Document);
        let mut driver = SearchDriver::default();
        driver.open(InPlace::Document);
        let options = ui::widget::FindOptions::default();
        driver.drive(&mut view, "build step 020", options, None, false);
        assert_eq!(view.find_test_matches(), vec![20]);
        assert_eq!(driver.position(), Some((1, 1)));
        assert!(
            view.scroll_offset() > 0,
            "find reveals a distant history row"
        );
        let abilities = driver.abilities();
        assert!(abilities.regex, "the bar offers regular expressions");
        assert!(!abilities.replace, "scrollback is not edited");
        let regex = ui::widget::FindOptions {
            mode: ui::widget::FindMode::Regex,
            ..options
        };
        driver.drive(&mut view, "step 04[2-3]", regex, None, false);
        assert_eq!(view.find_test_matches(), vec![42, 43]);
        driver.drive(&mut view, "step 04[2-3]", regex, None, true);
        assert!(
            view.find_test_matches().is_empty(),
            "closing forgets the matches"
        );
        assert!(
            drain_writes(&writes).is_empty(),
            "Find never writes to the PTY"
        );
        assert_eq!(
            doc.get_heads(),
            heads_before,
            "find text/matches stay local rather than mutating the collaborative transcript"
        );
    }
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "cases merged from separate tests; each case stays self-contained"
)]
fn context_menu_is_keyboard_navigable_and_laid_out_in_view() {
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
            widget.draw_child(
                &mut Scene::new(),
                Affine::IDENTITY,
                &mut TextRenderer::new(),
                &Theme::dark().with_text_scale(2.0),
                &PresenceCtx::empty(),
            );
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
                "the Find row asks the hosting view for the window bar's search"
            );
            assert!(
                ui::widget::controls::take_control_invocation(&mut widget).is_none(),
                "Find is local presentation"
            );
            assert!(drain_writes(&writes).is_empty());
        }
    }

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

        let mut scene = Scene::new();
        let mut text_renderer = TextRenderer::new();
        let theme = Theme::dark().with_text_scale(2.0);
        widget.draw_child(
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

#[test]
fn grid_mirror_stays_linear_aligned_and_reports_changes() {
    {
        const LINES: usize = 80;
        let (mut w, _write_rx, _out_tx) = build_test_widget_io();
        let doc = AutoCommit::new();
        w.force_geometry(Rect::new(0.0, 0.0, 1290.0, 760.0), 10.0, 20.0);

        let mut payload = Vec::new();
        for i in 0..LINES {
            payload.extend_from_slice(
            format!("\x1b[38;5;250m{i:>4}\x1b[0m  the quick brown fox jumps over the lazy dog {i:03}\r\n")
                .as_bytes(),
        );
        }

        w.feed(&payload);
        w.sync(&doc, &automerge::ROOT);

        let visible = w.visible_lines();
        assert!(
            visible.iter().any(|l| l.contains("lazy dog 079")),
            "newest bulk line rendered: {visible:?}"
        );
        w.scroll_to_top();
        let top = w.visible_lines();
        assert!(
            top.iter().any(|l| l.contains("lazy dog 000")),
            "oldest bulk line retained: {top:?}"
        );
    }

    {
        use vello::kurbo::Rect;
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
        let text = w.grid_text();
        let count = text.lines().count();
        assert!(count <= 120 + 24, "history bounded, got {count} lines");
        assert!(
            text.lines().any(|l| l.trim_end() == "ROW0149"),
            "newest line present in the aligned mirror"
        );
        assert!(
            !text.contains("ROW0000"),
            "oldest line trimmed off the front"
        );
        assert!(
            text.lines().any(|l| l.trim_end() == "ROW0120"),
            "a surviving scrollback line stays intact: {:?}",
            text.lines().rev().take(5).collect::<Vec<_>>()
        );
    }

    {
        use automerge::ROOT;
        use ui::State;

        let mut state = State::new();
        let _ = state.commit_action_authored(None, None, None);

        assert!(
            !state.commit_action_authored(None, None, None),
            "an empty transaction must report no change (App then skips save)"
        );
        ui::tilo_resource::write_key(
            &mut state.doc,
            &ROOT,
            "k",
            &ui::tilo_resource::Atom("v".into()),
        )
        .unwrap();
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

#[test]
fn paste_writes_to_pty_with_newline_and_bracketing_rules() {
    {
        let (mut w, write_rx) = build_test_widget();
        w.feed(b""); // init crdt / modes
        w.paste("echo hi\nls");
        let got = drain_writes(&write_rx);
        assert_eq!(got, b"echo hi\rls", "plain paste normalises LF to CR");
    }

    {
        let (mut w, write_rx) = build_test_widget();
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

#[test]
fn terminal_modes_switch_screens_cursor_paste_and_focus_reporting() {
    {
        use vello::kurbo::Rect;
        let (mut w, _rx) = build_test_widget();
        let doc = AutoCommit::new();
        w.force_geometry(Rect::new(0.0, 0.0, 800.0, 480.0), 10.0, 20.0);

        w.feed(b"MAIN CONTENT\r\n");
        w.sync(&doc, &automerge::ROOT);
        assert!(!w.is_alt_screen());
        assert!(w.grid_row(0).starts_with("MAIN CONTENT"));

        w.feed(b"\x1b[?1049h\x1b[2J\x1b[H");
        w.feed(b"\x1b[3;5HTUI REDRAW");
        w.sync(&doc, &automerge::ROOT);
        assert!(w.is_alt_screen(), "?1049h enters the alternate screen");
        assert!(
            w.grid_row(2).contains("TUI REDRAW"),
            "row 2 (1-based row 3): {:?}",
            w.grid_row(2)
        );
        assert!(
            !w.grid_row(0).contains("MAIN CONTENT"),
            "alt buffer is fresh"
        );

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

    {
        let (mut w, _rx) = build_test_widget();
        w.feed(b"");
        assert!(w.modes().cursor_visible, "cursor visible by default");

        w.feed(b"\x1b[?25l");
        assert!(!w.modes().cursor_visible, "?25l hides the cursor");
        w.feed(b"\x1b[?25h");
        assert!(w.modes().cursor_visible, "?25h shows the cursor");

        w.feed(b"\x1b[?1000h\x1b[?1006h");
        assert!(
            w.modes().mouse_report && w.modes().mouse_sgr,
            "mouse modes are tracked"
        );
        assert_eq!(w.modes().mouse_tracking, 1000);
    }

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

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "cases merged from separate tests; each case stays self-contained"
)]
fn mouse_tracking_padding_and_grid_hit_testing() {
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

    {
        let (mut w, _) = build_test_widget();
        w.place_at(Rect::new(0.0, 0.0, 1000.0, 640.0));
        let gr = w.grid_rect();
        assert!(gr.y0 > 0.0, "grid top inset by body padding: {gr:?}");
        assert!(gr.x0 > 0.0, "grid left inset by body padding: {gr:?}");
        assert!(gr.x1 < 1000.0, "grid right inset by padding: {gr:?}");
        assert!(gr.y1 < 640.0, "grid bottom inset by body padding: {gr:?}");
        let (cols, rows) = w.grid_size();
        assert!(cols > 0 && rows > 0, "reflow produced a usable grid");
        assert!(
            (rows as f64) < 640.0 / w.grid_char_h(),
            "body padding must reduce the row count below the naive full-height fit"
        );
    }

    {
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

    {
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

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "cases merged from separate tests; each case stays self-contained"
)]
fn ime_and_accessibility_expose_terminal_semantics() {
    {
        use winit::event::WindowEvent;

        let (mut w, rx) = build_test_widget();
        let mut doc = AutoCommit::new();
        w.set_focus(true);
        assert_eq!(w.ime_content(), Some(ui::ime::InputContent::TERMINAL));
        assert_eq!(
            ui::ime::ImeWindowState::from_widget(&w).unwrap().content,
            ui::ime::InputContent::TERMINAL
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

    {
        let (widget, rx) = build_test_widget();
        let mut app = ui::headless::FrameHarness::new(State::new(), Box::new(widget), (800, 480));
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

    {
        use ui::a11y::Action;
        use ui::widget::Widget;

        let (mut w, writes) = build_test_widget();
        let mut doc = AutoCommit::new();
        assert!(!Widget::perform_action(
            &mut w,
            &Action::SetValue("printf semantic".to_string()),
            &[],
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
        ));
        assert!(
            drain_writes(&writes).is_empty(),
            "a read-only transcript must never interpret SetValue as PTY input"
        );
    }

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
            &[],
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
            &[],
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

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "cases merged from separate tests; each case stays self-contained"
)]
fn terminal_resource_identity_convergence_and_presence() {
    {
        use ui::tilo_resource::Replica;

        let mut left = Replica::<crate::resource::Terminal>::new();
        let mut right = left.fork();
        for replica in [&mut left, &mut right] {
            replica
                .apply(
                    crate::resource::Terminal::CLAIM_PANE
                        .with((uuid::Uuid::new_v4().to_string(), None::<String>)),
                )
                .unwrap();
        }
        left.sync_with(&mut right)
            .expect("merge concurrent pane claims");
        let left = left.state().unwrap();
        let right = right.state().unwrap();
        assert_eq!(left.pane_id(), right.pane_id(), "pane identities converge");
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

    {
        use ui::widget::Widget;

        let (mut widget, _writes) = build_test_widget();
        widget.force_geometry(Rect::new(0.0, 0.0, 800.0, 120.0), 10.0, 20.0);
        widget.feed(
            b"\x1b]2;build session\x07\x1b]8;;https://example.test/docs\x1b\\docs\x1b]8;;\x1b\\",
        );
        assert_eq!(widget.title(), "build session");
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

    {
        use ui::presence::PresenceKey;
        let mut a = ui::framework::testing::TestApp::<crate::resource::TerminalShell>::new()
            .expect("terminal");
        a.with_surface(|terminal: &mut crate::TerminalView| terminal.feed(b"peer-a-line-one\r\n"))
            .expect("hosted terminal");
        a.settle();
        let joined = crate::resource::Terminal::read(&a.peer_document()).unwrap();
        let (mut b, _rx_b) = build_test_widget();
        ResourceSurface::<crate::resource::Terminal, crate::resource::TerminalEffect>::present(
            &mut b,
            &Rc::new(joined),
        );
        assert!(
            !b.grid_text().contains("peer-a-line-one"),
            "A's output is not in the record; B grid:\n{}",
            b.grid_text()
        );
        let a_key = a
            .with_surface(|terminal: &mut crate::TerminalView| {
                ui::presence::capability::presence_key(terminal)
            })
            .expect("hosted terminal");
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

    {
        use ui::collaboration::PresencePayload;

        let (mut w, _) = build_test_widget();
        w.force_grid(vec!["abcdefgh".to_string()]);

        w.force_selection(Some(Selection {
            anchor: GridPos { row: 0, col: 2 },
            cursor: GridPos { row: 0, col: 5 },
        }));
        let (_key, payload) =
            ui::presence::capability::presence_payload(&w).expect("payload with selection");
        match payload {
            PresencePayload::Selection { start, end } => {
                assert_eq!((start, end), (2, 5), "selection linearised to cell columns");
            }
            other => panic!("expected Selection payload, got {other:?}"),
        }

        w.force_selection(None);
        let (_key, payload) =
            ui::presence::capability::presence_payload(&w).expect("payload with cursor");
        assert!(
            matches!(payload, PresencePayload::Caret { .. }),
            "cursor payload"
        );
    }
}

fn cells(w: &TerminalWidget, row: usize, col: usize, len: usize) -> Vec<(char, Attr)> {
    w.grid_row(row)
        .chars()
        .skip(col)
        .take(len)
        .enumerate()
        .map(|(i, c)| (c, w.cell_attr(row, col + i)))
        .collect()
}

#[test]
fn claude_codes_start_up_leaves_its_text_without_a_line_under_it() {
    use vello::kurbo::Rect;
    let (mut w, _rx) = build_test_widget();
    let doc = AutoCommit::new();
    w.force_geometry(Rect::new(0.0, 0.0, 800.0, 480.0), 10.0, 20.0);

    w.feed(
        b"\x1b7\x1b[r\x1b8\x1b[?25h\x1b[?2004h\x1b[?2031h\x1b[?1004h\x1b[<u\x1b[>5u\x1b[>4;2m\
          \x1b]0;\xe2\x9c\xb3 Claude Code\x07\x1b[?1049h\x1b[2J\x1b[H\x1b[<u\x1b[>5u\x1b[>4;2m\
          \x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1006h\x1b[?25l\x1b[H\
          \x1b[1mClaude Code\x1b[22m\x1b[13G\x1b[38;5;246mv2.1.305\x1b[39m",
    );
    w.sync(&doc, &automerge::ROOT);

    let drawn = cells(&w, 0, 0, 20);
    let text: String = drawn.iter().map(|(c, _)| c).collect();
    assert_eq!(
        text, "Claude Code v2.1.305",
        "the private-marker sequences moved nothing: {text:?}"
    );
    for (c, attr) in &drawn {
        assert!(!attr.underline, "{c:?} has a line drawn under it: {attr:?}");
        assert!(!attr.dim, "{c:?} is drawn faint: {attr:?}");
    }
    assert!(
        drawn.iter().take(6).all(|(_, a)| a.bold),
        "the bold that was asked for still applies"
    );
}

#[test]
fn kitty_keyboard_pushes_and_pops_do_not_restore_the_cursor() {
    use vello::kurbo::Rect;
    let (mut w, _rx) = build_test_widget();
    let doc = AutoCommit::new();
    w.force_geometry(Rect::new(0.0, 0.0, 800.0, 480.0), 10.0, 20.0);

    w.feed(b"\x1b[3;10H\x1b[4m\x1b7\x1b[24m\x1b[H\x1b[>5uA\x1b[<uB\x1b[=1;1uC");
    w.sync(&doc, &automerge::ROOT);

    let drawn = cells(&w, 0, 0, 3);
    let text: String = drawn.iter().map(|(c, _)| c).collect();
    assert_eq!(text, "ABC", "each letter follows the last: {text:?}");
    assert!(drawn.iter().all(|(_, a)| !a.underline), "{drawn:?}");
}

#[test]
fn underline_ends_where_programs_end_it() {
    use vello::kurbo::Rect;
    let (mut w, _rx) = build_test_widget();
    let doc = AutoCommit::new();
    w.force_geometry(Rect::new(0.0, 0.0, 800.0, 480.0), 10.0, 20.0);

    let cases: &[(&[u8], bool, bool)] = &[
        (b"\x1b[4m", true, false),
        (b"\x1b[4m\x1b[24m", false, false),
        (b"\x1b[4m\x1b[0m", false, false),
        (b"\x1b[4:3m", true, false),
        (b"\x1b[4:3m\x1b[4:0m", false, false),
        (b"\x1b[4:0m", false, false),
        (b"\x1b[58;2;2;4;4m", false, false),
        (b"\x1b[58;5;4m", false, false),
        (b"\x1b[58:2::4:2:4m", false, false),
        (b"\x1b[4;58;2;1;2;3m\x1b[59m\x1b[24m", false, false),
        (b"\x1b[>4;2m", false, false),
        (b"\x1b[>4m", false, false),
    ];
    for (i, (seq, underline, dim)) in cases.iter().enumerate() {
        let mut bytes = b"\x1b[0m\x1b[H".to_vec();
        bytes.extend_from_slice(seq);
        bytes.push(b'X');
        w.feed(&bytes);
        w.sync(&doc, &automerge::ROOT);
        let a = w.cell_attr(0, 0);
        assert_eq!(
            (a.underline, a.dim),
            (*underline, *dim),
            "case {i} {:?}: {a:?}",
            String::from_utf8_lossy(seq)
        );
    }

    w.feed(b"\x1b[0m\x1b[2;1H\x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\ plain");
    w.sync(&doc, &automerge::ROOT);
    let row = cells(&w, 1, 0, 10);
    assert!(row.iter().take(4).all(|(_, a)| a.link_id != 0), "{row:?}");
    assert!(
        row.iter()
            .skip(4)
            .all(|(_, a)| a.link_id == 0 && !a.underline),
        "text after the link's close is plain: {row:?}"
    );
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "cases merged from separate tests; each case stays self-contained"
)]
fn sgr_and_graphics_escapes_parse_into_cells_and_placements() {
    {
        use crate::CellColor;
        use vello::kurbo::Rect;
        let (mut w, _rx) = build_test_widget();
        let doc = AutoCommit::new();
        w.force_geometry(Rect::new(0.0, 0.0, 800.0, 480.0), 10.0, 20.0);

        w.feed(b"\x1b[H\x1b[38;2;10;20;30mA");
        w.sync(&doc, &automerge::ROOT);
        assert_eq!(
            w.cell_attr(0, 0).fg,
            CellColor::Rgb(10, 20, 30),
            "38;2 truecolor fg"
        );

        w.feed(b"\x1b[H\x1b[38;5;196mB");
        w.sync(&doc, &automerge::ROOT);
        assert_eq!(
            w.cell_attr(0, 0).fg,
            CellColor::Indexed(196),
            "38;5 indexed fg"
        );

        w.feed(b"\x1b[H\x1b[38:5:214mC");
        w.sync(&doc, &automerge::ROOT);
        assert_eq!(
            w.cell_attr(0, 0).fg,
            CellColor::Indexed(214),
            "38:5 colon-form indexed fg"
        );

        w.feed(b"\x1b[H\x1b[48;2;5;6;7mD");
        w.sync(&doc, &automerge::ROOT);
        assert_eq!(
            w.cell_attr(0, 0).bg,
            CellColor::Rgb(5, 6, 7),
            "48;2 truecolor bg"
        );
    }

    {
        use crate::CellColor;
        use vello::kurbo::Rect;
        let (mut w, _rx) = build_test_widget();
        let doc = AutoCommit::new();
        w.force_geometry(Rect::new(0.0, 0.0, 800.0, 480.0), 10.0, 20.0);

        w.feed(b"\x1b[H\x1b[1;3;4;7mW");
        w.sync(&doc, &automerge::ROOT);
        let a = w.cell_attr(0, 0);
        assert!(
            a.bold && a.italic && a.underline && a.reverse,
            "combined SGR style bits: {a:?}"
        );

        w.feed(b"\x1b[H\x1b[0mN");
        w.sync(&doc, &automerge::ROOT);
        let b = w.cell_attr(0, 0);
        assert_eq!(b.fg, CellColor::Default);
        assert!(
            !b.bold && !b.italic && !b.underline && !b.reverse,
            "SGR 0 resets style: {b:?}"
        );
    }

    {
        use vello::kurbo::Rect;
        let (mut w, _rx) = build_test_widget();
        let doc = AutoCommit::new();
        w.force_geometry(Rect::new(0.0, 0.0, 800.0, 480.0), 10.0, 20.0);

        w.feed(b"\x1b[H\x1b[2;9mX");
        w.sync(&doc, &automerge::ROOT);
        let a = w.cell_attr(0, 0);
        assert!(a.dim, "SGR 2 sets faint: {a:?}");
        assert!(a.strike, "SGR 9 sets crossed-out: {a:?}");

        w.feed(b"\x1b[H\x1b[1;2;9mY\x1b[22;29mZ");
        w.sync(&doc, &automerge::ROOT);
        let z = w.cell_attr(0, 1);
        assert!(!z.dim && !z.bold, "SGR 22 clears bold+faint: {z:?}");
        assert!(!z.strike, "SGR 29 clears crossed-out: {z:?}");
    }

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
    }
}

fn pump_until_contains(w: &mut TerminalWidget, doc: &mut AutoCommit, needle: &str) -> bool {
    let deadline = ui::time::now() + hang_bound();
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

#[test]
fn spawned_pty_sessions_run_commands_in_a_fresh_grid() {
    {
        let cmd = shell_command("printf 'TILO_PTY_OK\\n'", "echo TILO_PTY_OK");
        let mut w = TerminalWidget::spawn_command(cmd).expect("spawn pty command");
        let mut doc = AutoCommit::new();
        assert!(
            pump_until_contains(&mut w, &mut doc, "TILO_PTY_OK"),
            "real PTY command output must reach the grid; grid was:\n{}",
            w.grid_text()
        );
    }

    {
        #[cfg(unix)]
        let home = tempfile::tempdir().expect("scratch HOME");
        #[cfg(unix)]
        let cmd = hermetic_shell_command("/bin/sh", home.path());
        #[cfg(windows)]
        let cmd = crate::CommandBuilder::new(tilo_process::login_shell::login_shell());
        let mut w = TerminalWidget::spawn_command(cmd).expect("spawn default shell");
        let mut doc = AutoCommit::new();
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

    {
        let w = TerminalWidget::spawn_command(shell_command("true", "exit /b 0")).expect("spawn");
        assert_eq!(w.grid_size(), (80, 24));
    }

    {
        use automerge::AutoCommit;

        let mut old = TerminalWidget::spawn_headless().expect("old terminal");
        let doc = AutoCommit::new();
        old.feed(b"old-session-output");
        old.sync(&doc, &automerge::ROOT);
        assert!(old.grid_text().contains("old-session-output"));

        let mut command = crate::CommandBuilder::new("/bin/sh");
        command.arg("-c");
        command.arg("printf new-session-output");
        let mut fresh = TerminalWidget::spawn_command(command).expect("new local terminal");
        tilo_test_env::wait_until("the new session's output on the grid", || {
            fresh.sync(&doc, &automerge::ROOT);
            fresh.grid_text().contains("new-session-output")
        });

        let screen = fresh.grid_text();
        assert!(
            !screen.contains("old-session-output"),
            "a new PTY must not inherit the previous process' screen: {screen:?}"
        );
    }

    {
        let shell = tilo_process::login_shell::login_shell();
        let cmd = crate::CommandBuilder::new(shell.clone());
        TerminalWidget::spawn_command(cmd)
            .unwrap_or_else(|error| panic!("a terminal opens on {shell}: {error}"));
    }

    {
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

#[cfg(unix)]
fn hermetic_shell_command(shell: &str, home: &std::path::Path) -> portable_pty::CommandBuilder {
    let mut cmd = crate::CommandBuilder::new(shell);
    let empty_rc = home.join("empty-rc");
    std::fs::write(&empty_rc, b"").expect("write empty rc file");
    cmd.env_clear();
    if let Some(path) = std::env::var_os("PATH") {
        cmd.env("PATH", path);
    }
    cmd.env("HOME", home);
    cmd.env("XDG_CONFIG_HOME", home);
    cmd.env("ZDOTDIR", home);
    cmd.env("ENV", &empty_rc);
    cmd.env("BASH_ENV", &empty_rc);
    cmd.cwd(home);
    cmd
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "cases merged from separate tests; each case stays self-contained"
)]
fn keyboard_encoding_sends_expected_sequences() {
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

    {
        use vello::kurbo::Rect;
        use winit::keyboard::{Key, NamedKey};
        let (mut w, rx) = build_test_widget();
        let mut doc = AutoCommit::new();
        w.force_geometry(Rect::new(0.0, 0.0, 800.0, 480.0), 10.0, 20.0);

        send_key_mods(
            &mut w,
            &mut doc,
            Key::Named(NamedKey::ArrowUp),
            false,
            false,
            false,
        );
        assert_eq!(drain_writes(&rx), b"\x1b[A", "default arrow = CSI A");

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
        for (key, shift) in [
            (Key::Named(NamedKey::Tab), false),
            (Key::Named(NamedKey::Tab), true),
            (Key::Character("T".into()), true),
        ] {
            send_key_mods(&mut w, &mut doc, key.clone(), true, shift, false);
            assert!(
                drain_writes(&rx).is_empty(),
                "{key:?} with Ctrl (Shift {shift}) is the desktop's, not the shell's"
            );
        }
        send_key_mods(
            &mut w,
            &mut doc,
            Key::Character("t".into()),
            true,
            false,
            false,
        );
        assert_eq!(drain_writes(&rx), [0x14], "Ctrl+T = ^T");
        send_key_mods(
            &mut w,
            &mut doc,
            Key::Character("w".into()),
            true,
            false,
            false,
        );
        assert_eq!(drain_writes(&rx), [0x17], "Ctrl+W = ^W");
    }

    {
        use vello::kurbo::Rect;
        use winit::keyboard::{Key, SmolStr};
        let (mut w, rx) = build_test_widget();
        let mut doc = AutoCommit::new();
        w.force_geometry(Rect::new(0.0, 0.0, 800.0, 480.0), 10.0, 20.0);

        send_key_mods(
            &mut w,
            &mut doc,
            Key::Character(SmolStr::new_static("f")),
            false,
            false,
            true,
        );
        assert_eq!(drain_writes(&rx), b"\x1bf", "Alt+f = ESC f (Meta)");

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

fn send_left_press_mods(widget: &mut TerminalWidget, doc: &mut AutoCommit, shift: bool) {
    use winit::keyboard::ModifiersState;
    let mut state = ModifiersState::empty();
    if shift {
        state |= ModifiersState::SHIFT;
    }
    let ev = ui::synthetic_input::mouse_press();
    ui::testing::Dispatch::new()
        .modifiers(state)
        .doc(doc)
        .send(widget, &ev);
}

fn send_middle_press(widget: &mut TerminalWidget, doc: &mut AutoCommit) {
    use winit::event::{ElementState, MouseButton};
    let ev = ui::synthetic_input::mouse_input(ElementState::Pressed, MouseButton::Middle);
    ui::testing::Dispatch::new().doc(doc).send(widget, &ev);
}

fn col_center_x(col: usize) -> f64 {
    4.0 + col as f64 * 10.0 + 5.0
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "cases merged from separate tests; each case stays self-contained"
)]
#[serial_test::serial]
fn mouse_selection_gestures_select_extend_and_stay_anchored() {
    {
        let (mut w, _) = build_test_widget();
        let mut doc = AutoCommit::new();
        w.force_geometry(Rect::new(0.0, 0.0, 800.0, 480.0), 10.0, 20.0);
        w.force_viewport_start(0);
        w.force_grid(vec!["hello world foobar".to_string()]);

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

    {
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

    {
        let (mut w, _) = build_test_widget();
        let mut doc = AutoCommit::new();
        w.force_geometry(Rect::new(0.0, 0.0, 800.0, 480.0), 10.0, 20.0);
        w.force_viewport_start(0);
        w.force_grid(vec!["hello world foobar".to_string()]);

        send_cursor_move(&mut w, &mut doc, col_center_x(2), 10.0);
        send_left_press(&mut w, &mut doc);
        send_cursor_move(&mut w, &mut doc, col_center_x(5), 10.0);
        send_left_release(&mut w, &mut doc);
        assert_eq!(
            w.test_selected_text().as_deref(),
            Some("llo"),
            "initial drag selection"
        );

        send_cursor_move(&mut w, &mut doc, col_center_x(10), 10.0);
        send_left_press_mods(&mut w, &mut doc, /*shift=*/ true);
        assert_eq!(
            w.test_selected_text().as_deref(),
            Some("llo worl"),
            "shift+click extends from the original anchor"
        );
    }

    {
        let (mut w, _write_rx) = build_test_widget();
        let mut doc = AutoCommit::new();
        w.force_geometry(Rect::new(0.0, 0.0, 800.0, 240.0), 10.0, 20.0); // 12 visible rows
        let mut grid: Vec<String> = (0..20).map(|i| format!("row{i:02} filler text")).collect();
        grid[5] = "alpha bravo charlie".to_string();
        w.force_grid(grid);
        w.force_viewport_start(5);
        w.set_select_to_copy(true);

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

        let sel = w.selection().expect("selection present");
        assert_eq!(
            sel.anchor.row, 5,
            "selection anchored to absolute buffer row 5"
        );

        w.force_viewport_start(0);
        assert_eq!(
            w.test_selected_text().as_deref(),
            Some("bravo"),
            "selection stays on the same buffer cells after scrolling"
        );
        w.force_viewport_start(8);
        assert_eq!(w.test_selected_text().as_deref(), Some("bravo"));

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

    {
        let (mut widget, _) = build_test_widget();
        let mut doc = AutoCommit::new();
        widget.force_geometry(Rect::new(0.0, 0.0, 800.0, 600.0), 10.0, 20.0);

        send_cursor_move(&mut widget, &mut doc, 4.0 + 2.0 * 10.0 + 1.0, 20.0 + 1.0);
        send_left_press(&mut widget, &mut doc);

        let ev = ui::synthetic_input::cursor_moved(4.0 + 20.0 * 10.0 + 1.0, 20.0 + 1.0);
        let handled = ui::testing::Dispatch::new()
            .doc(&mut doc)
            .send(&mut widget, &ev)
            .handled;

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

#[test]
fn osc_sequences_update_title_and_hyperlinks() {
    {
        use ui::AppWidget;
        let (mut w, _) = build_test_widget();
        w.feed(b"\x1b]2;my session\x07");
        assert_eq!(w.title(), "my session", "OSC 2 sets the window title");
        assert_eq!(
            w.app_title(),
            "my session",
            "app_title reflects the OSC title"
        );

        w.feed(b"\x1b]0;another\x1b\\");
        assert_eq!(w.title(), "another", "OSC 0 sets the window title too");
    }

    {
        assert_eq!(
            crate::inner::safe_osc_text(b"build 42", 32).as_deref(),
            Some("build 42")
        );
        assert!(crate::inner::safe_osc_text(b"line one\nline two", 32).is_none());
        assert!(crate::inner::safe_osc_text(&[b'x'; 33], 32).is_none());
        assert!(crate::inner::safe_osc_text(&[0xff], 32).is_none());
    }

    {
        let (mut w, _) = build_test_widget();
        w.force_geometry(Rect::new(0.0, 0.0, 800.0, 480.0), 10.0, 20.0);
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
    }
}

#[test]
#[cfg(target_os = "linux")]
#[serial_test::serial]
fn osc_52_writes_clipboard_from_base64() {
    let (mut w, _) = build_test_widget();
    ui::text_selection::clipboard_copy("untouched");
    w.feed(b"\x1b]52;c;dGlsby1vc2M1Mg==\x07");
    assert_eq!(
        ui::text_selection::clipboard_paste().as_deref(),
        Some("untouched"),
        "by default, program output must not replace the clipboard"
    );
    w.set_allow_osc52_clipboard(true);
    w.feed(b"\x1b]52;c;dGlsby1vc2M1Mg==\x07");
    assert_eq!(
        ui::text_selection::clipboard_paste().as_deref(),
        Some("tilo-osc52"),
        "once allowed, OSC 52 decodes base64 into the clipboard"
    );
}

#[test]
fn device_attribute_queries_reply() {
    {
        let (mut w, write_rx) = build_test_widget();
        w.feed(b"\x1b[c");
        let got = drain_writes(&write_rx);
        assert_eq!(
            got, b"\x1b[?6c",
            "DA1 (CSI c) must answer with the device attributes"
        );
    }

    {
        let (mut w, write_rx) = build_test_widget();
        w.feed(b"\x1b[0c");
        assert_eq!(drain_writes(&write_rx), b"\x1b[?6c", "CSI 0 c is DA1 too");
    }

    {
        let (mut w, write_rx) = build_test_widget();
        w.feed(b"\x1bZ");
        assert_eq!(
            drain_writes(&write_rx),
            b"\x1b[?6c",
            "ESC Z (DECID) is a DA1 request"
        );
    }

    {
        let (mut w, write_rx) = build_test_widget();
        w.feed(b"\x1b[>c");
        let reply = String::from_utf8(drain_writes(&write_rx)).expect("ascii reply");
        assert!(
            reply.starts_with("\x1b[>0;") && reply.ends_with(";1c"),
            "DA2 (CSI > c) reports terminal type and version: {reply:?}"
        );
    }
}

#[test]
fn status_and_mode_reports_reply_or_stay_silent() {
    {
        let (mut w, write_rx) = build_test_widget();
        w.feed(b"\x1b[5;3H");
        let _ = drain_writes(&write_rx); // CUP itself sends nothing; clear.
        w.feed(b"\x1b[6n");
        assert_eq!(
            drain_writes(&write_rx),
            b"\x1b[5;3R",
            "DSR 6n (CPR) reports the 1-based cursor row;col"
        );
    }

    {
        let (mut w, write_rx) = build_test_widget();
        w.feed(b"\x1b[5n");
        assert_eq!(drain_writes(&write_rx), b"\x1b[0n", "DSR 5n → terminal OK");
    }

    {
        let (mut w, write_rx) = build_test_widget();
        w.feed(b"\x1b[?2004h\x1b[?2004$p");
        assert_eq!(
            drain_writes(&write_rx),
            b"\x1b[?2004;1$y",
            "DECRQM must report bracketed paste as set (1) after ?2004h"
        );
        w.feed(b"\x1b[?2004l\x1b[?2004$p");
        assert_eq!(
            drain_writes(&write_rx),
            b"\x1b[?2004;2$y",
            "DECRQM must report bracketed paste as reset (2) after ?2004l"
        );
    }

    {
        let (mut w, write_rx) = build_test_widget();
        w.feed(b"\x1b[?9999$p");
        assert_eq!(drain_writes(&write_rx), b"\x1b[?9999;0$y");
    }

    {
        let (mut w, write_rx) = build_test_widget();
        w.feed(b"\x1b[2 q");
        assert!(
            drain_writes(&write_rx).is_empty(),
            "a non-query CSI must not write a spurious reply to the shell"
        );
    }
}

#[test]
#[cfg(unix)]
fn real_pty_query_replies_and_fish_startup() {
    fn fish_starts_without_the_device_attribute_warning() {
        let Some(fish) = locate_fish() else {
            eprintln!("fish not installed; skipping live-fish startup test");
            return;
        };

        let mut cmd = crate::CommandBuilder::new(fish);
        cmd.arg("--no-config");
        cmd.arg("-i");
        let mut w = TerminalWidget::spawn_command(cmd).expect("spawn fish under a pty");
        let mut doc = AutoCommit::new();

        tilo_test_env::wait_until("fish to show its prompt", || {
            w.sync(&doc, &ROOT);
            !w.grid_text().trim().is_empty()
        });

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

    {
        use portable_pty::CommandBuilder;
        let mut cmd = CommandBuilder::new("/bin/sh");
        cmd.arg("-c");
        cmd.arg("stty -icanon -echo min 1 time 0; printf '\\033[c'; head -c 5 | cat -v; echo; echo REPLY_DONE");
        let mut w = TerminalWidget::spawn_command(cmd).expect("spawn pty command");
        let mut doc = AutoCommit::new();
        assert!(
            pump_until_contains(&mut w, &mut doc, "REPLY_DONE"),
            "the child must receive the DA1 reply over the real PTY; grid was:\n{}",
            w.grid_text()
        );
        assert!(
            w.grid_text().contains("[?6c"),
            "the child echoed back the exact DA1 reply bytes; grid was:\n{}",
            w.grid_text()
        );
    }

    fish_starts_without_the_device_attribute_warning();
}

#[cfg(unix)]
fn locate_fish() -> Option<std::path::PathBuf> {
    tilo_process::tools::ToolDirs::current().find("fish")
}

#[test]
fn confinement_routes_are_mandatory_for_hosted_terminals() {
    {
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

    {
        use crate::inner::{ConfineRoute, decide_route};
        use crate::target::SessionTarget;
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
        assert_eq!(decide_route(&env, true, false, None), ConfineRoute::Service);
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

    {
        let result = crate::TerminalWidget::spawn_hosted(crate::HostedProcessServices::default());
        assert!(matches!(
            result,
            Err(crate::HostedTerminalError::RequiredServiceMissing("ush"))
        ));
    }
}

#[cfg(unix)]
pub(crate) fn private_ush_listener(runtime: &std::path::Path) -> std::os::unix::net::UnixListener {
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

#[cfg(windows)]
#[test]
#[serial_test::serial]
fn windows_native_and_hosted_terminal_paths() {
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

    {
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set(crate::inner::USH_SERVICE_FD_ENV, "3");
        let result = TerminalWidget::spawn_default();
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
fn deleting_lines_on_screen_leaves_the_scrollback_alone() {
    let (mut w, _rx) = build_test_widget();
    let doc = AutoCommit::new();
    w.force_geometry(Rect::new(0.0, 0.0, 800.0, 240.0), 10.0, 20.0);
    let history: String = (0..60).fold(String::new(), |mut history, n| {
        use std::fmt::Write as _;
        let _ = write!(history, "H{n:03}\r\n");
        history
    });
    w.feed(history.as_bytes());
    w.sync(&doc, &ROOT);
    let before = w.grid_text();
    w.feed(b"\x1b[5;1H\x1b[2M");
    w.sync(&doc, &ROOT);
    let after = w.grid_text();
    let scrollback = |text: &str| text.lines().take(30).map(str::to_owned).collect::<Vec<_>>();
    assert_eq!(scrollback(&after), scrollback(&before));
    assert!(after.contains("H000") && after.contains("H059"), "{after}");
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
        w.grid_row(1).trim().is_empty(),
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
            widget.draw_child(
                scene,
                Affine::IDENTITY,
                &mut text,
                &theme,
                &PresenceCtx::empty(),
            );
        });
        (scene, parts.expect("the terminal's parts are well nested"))
    };
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
        "#3892 terminal keystroke damage: {area} of {} px",
        viewport.area()
    );
    assert!(
        area * 4.0 < viewport.area(),
        "typing repaints a sliver of the terminal, not {area} of {} px: {damage:?}",
        viewport.area()
    );
}

#[test]
fn fingers_scroll_the_scrollback_by_the_pixel_and_copy_its_rows() {
    use ui::{Theme, presence::PresenceCtx, text_render::TextRenderer};
    use vello::{Scene, kurbo::Affine};
    use winit::event::{MouseScrollDelta, TouchPhase, WindowEvent};

    let (mut widget, _) = build_test_widget();
    let viewport = Rect::new(0.0, 0.0, 800.0, 400.0);
    widget.force_geometry(viewport, 10.0, 20.0);
    let mut output = Vec::new();
    for line in 0..200 {
        output.extend_from_slice(format!("line {line:03}\r\n").as_bytes());
    }
    widget.feed(&output);
    let theme = Theme::dark();
    let mut text = TextRenderer::new();
    let mut frame = |widget: &mut TerminalWidget| {
        widget.draw_child(
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut text,
            &theme,
            &PresenceCtx::empty(),
        );
    };
    frame(&mut widget);
    widget.feed(b"");
    frame(&mut widget);
    widget.mark_drawn();
    let mut doc = AutoCommit::new();
    let mut fingers = |widget: &mut TerminalWidget, dy: f64| {
        let event = WindowEvent::MouseWheel {
            device_id: winit::event::DeviceId::dummy(),
            delta: MouseScrollDelta::PixelDelta(winit::dpi::PhysicalPosition::new(0.0, dy)),
            phase: TouchPhase::Moved,
        };
        ui::testing::Dispatch::new()
            .doc(&mut doc)
            .send(widget, &event);
    };
    let centre = viewport.center();
    ui::testing::Dispatch::new().send(
        &mut widget,
        &WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: winit::dpi::PhysicalPosition::new(centre.x, centre.y),
        },
    );
    fingers(&mut widget, 7.0);
    assert_eq!(
        widget.scroll_offset(),
        1,
        "seven pixels back show part of the row above the live screen"
    );
    frame(&mut widget);
    let mut regions = Vec::new();
    let offers = ui::scroll_copy::accepting(|| widget.collect_damage(&mut regions));
    assert_eq!(offers.len(), 1, "the scroll is offered as a copy");
    assert_eq!(offers[0].delta(), vello::kurbo::Vec2::new(0.0, 7.0));
    let area: f64 = regions.iter().map(Rect::area).sum();
    assert!(
        area * 4.0 < viewport.area(),
        "a scroll repaints a strip of the terminal, not {area} of {} px: {regions:?}",
        viewport.area()
    );
    widget.mark_drawn();
    fingers(&mut widget, -7.0);
    assert_eq!(widget.scroll_offset(), 0);
    let lines = widget.visible_lines();
    assert!(
        lines.iter().any(|line| line.contains("line 199")),
        "the live output is in view: {lines:?}"
    );
}

#[allow(
    clippy::float_cmp,
    reason = "a bell that is off leaves the flash at exactly 0.0, untouched"
)]
#[test]
fn the_bell_flashes_the_pane_when_it_is_turned_on() {
    use crate::preferences::{Bell, Preferences};

    let (mut widget, _writes) = build_test_widget();
    widget.use_preferences(Preferences::none());
    widget.feed(b"ready\x07");
    assert_eq!(
        widget.bell_flash_for_test(),
        0.0,
        "the bell is off by default: nothing happens"
    );

    widget.set_bell(Bell::Visual);
    widget.feed(b"\x07");
    assert!(
        widget.bell_flash_for_test() > 0.0,
        "the bell flashes the pane"
    );
    assert!(
        widget.needs_animation_frame(),
        "the flash keeps frames coming while it fades"
    );
}

#[test]
fn copy_on_select_is_kept_for_the_next_terminal() {
    use crate::preferences::Preferences;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_path_buf();
    let (mut first, _writes) = build_test_widget();
    first.use_preferences(Preferences::at(&path));
    assert!(!first.selects_to_copy_for_test(), "off until asked");
    first.set_select_to_copy(true);

    let (mut second, _writes) = build_test_widget();
    second.use_preferences(Preferences::at(&path));
    assert!(
        second.selects_to_copy_for_test(),
        "a new terminal opens with the choice already made"
    );
}

#[test]
#[serial_test::serial]
fn an_agents_narrated_run_lands_at_the_prompt() {
    let mut app =
        ui::framework::testing::TestApp::<crate::resource::TerminalShell>::new().expect("terminal");
    app.settle();
    let prompt = app
        .with_surface(|terminal: &mut crate::TerminalView| terminal.prompt_rect())
        .expect("hosted terminal");
    assert!(
        prompt.width() > 0.0 && prompt.height() > 0.0,
        "the prompt has a line: {prompt:?}"
    );
    let bindings = ui::widget::controls::control_action_bindings(app.harness().root_widget());
    let run = bindings
        .iter()
        .find_map(|binding| match &binding.kind {
            ui::widget::ControlBindingKind::Action(address) if address.action == "run" => {
                Some(address.clone())
            }
            _ => None,
        })
        .expect("the prompt is bound to the session's run");
    let event = ui::headless::agent_acting(&run.resource, &run.action);
    let locus = app
        .harness()
        .semantic_locus(&event)
        .expect("the run is shown at the prompt");
    assert!(
        (locus.center() - prompt.center()).length() < 0.5,
        "the cursor is on the prompt's line: {locus:?} vs {prompt:?}"
    );
    app.harness()
        .update_presence(|presence| presence.apply_interaction(event.clone()).is_ok());
    let mut followed = ui::headless::FollowedView::default();
    assert!(
        !app.harness()
            .follow_view("someone-else", &mut ui::headless::FollowedView::default()),
        "not following moves nothing"
    );
    assert!(app.harness().follow_view("agent", &mut followed));
    assert!(!app.harness().follow_view("agent", &mut followed));
    let audit = app.harness().native_app_audit();
    assert!(audit.is_certified(), "{audit:#?}");
}
