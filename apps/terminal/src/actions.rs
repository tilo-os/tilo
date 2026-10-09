//! The terminal's actions and effects.
//!
//! A terminal session is a record ([`Terminal`](crate::resource::Terminal)):
//! what its process shows, the title the program set, where the session runs
//! and the pane collaborators' presence is scoped to. The emulator parses the
//! process's output onto its own screen, and after each batch the difference
//! reaches the record as one [`Output`](crate::resource::TerminalAction::Output)
//! action, so every collaborator and every past version sees the same screen.
//! None of these actions is an undo step: they record what a process did,
//! which no undo takes back.
//!
//! Keystrokes are not actions: they travel straight to the process's stdin
//! and change no document. Driving the process from the palette, an agent
//! or a script — send text, run a command, interrupt it, paste — is an
//! effect on the live session. Presentation (the scrollback offset, the
//! selection, the find bar, the context menu) is view state of the widget.

use ui::tilo_actions::ActionRegistry;

/// Install the terminal session action decoder on the live launch path.
pub fn register(registry: &mut ActionRegistry) {
    <crate::resource::Terminal as ui::tilo_resource::Resource>::register(registry);
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::sync::{Arc, Mutex};

    use automerge::{AutoCommit, ROOT};
    use portable_pty::{MasterPty, PtySize};
    use std::sync::mpsc::Receiver;
    use taffy::{NodeId, TaffyTree};
    use winit::event::{Modifiers, WindowEvent};

    use ui::widget::Widget;

    use crate::TerminalWidget;
    use crate::resource::TerminalEffect;
    use ui::tilo_resource::Effect as _;

    /// A PTY master with no process behind it: writes are dropped, reads return
    /// EOF. Enough for the emulator to seed its grid and accept input.
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

    /// Construct a terminal over a mock PTY whose *writer* funnels every byte the
    /// production input path sends to the shell into a channel, so a test can
    /// assert exactly what reached the shell's stdin. This is the same rig the
    /// widget's own tests use — the real `handle_event` → `write_input` path runs
    /// unchanged.
    fn build_test_widget() -> (TerminalWidget, Receiver<Vec<u8>>) {
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
            Arc::new(Mutex::new(Box::new(MockMaster))),
            pty_out_rx,
            tilo_content::ContentStore::memory(),
        );
        (widget, write_rx)
    }

    /// Everything the PTY-write channel has buffered, concatenated — i.e. every
    /// byte the terminal has sent to the shell so far.
    fn drain_writes(rx: &Receiver<Vec<u8>>) -> Vec<u8> {
        let mut out = Vec::new();
        while let Ok(b) = rx.try_recv() {
            out.extend_from_slice(&b);
        }
        out
    }

    use ui::synthetic_input::char_key;

    /// Pump one event through the production `Widget::handle_event`.
    fn send(widget: &mut TerminalWidget, doc: &mut AutoCommit, ev: &WindowEvent) {
        let taffy = TaffyTree::<()>::new();
        let mut handled = false;
        let mut focus = None;
        widget.handle_event(
            ev,
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &Modifiers::default(),
                state: ui::widget::Edits::lend(doc),
                path: &ROOT,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: NodeId::new(0),
                taffy: &taffy,
            },
        );
    }

    /// Keystrokes reach the shell and change no document.
    #[test]
    fn keys_go_to_the_shell_not_the_document() {
        // Case `keystrokes_go_to_the_shell_not_the_document`.
        // A keystroke is **external**: driven through the production
        // `Widget::handle_event`, the character reaches the shell's stdin (arrives on
        // the PTY-write channel) while the shared document's heads are **unchanged**
        // — the input path holds only the PTY writer and takes no `AutoCommit`, so it
        // writes nothing to the CRDT. Hence typing is not an action.
        {
            let (mut widget, write_rx) = build_test_widget();
            let mut doc = AutoCommit::new();

            doc.commit();
            let heads_before = doc.get_heads();
            let _ = drain_writes(&write_rx); // ignore anything queued during seeding

            // Press 'a' through the production keyboard path.
            send(&mut widget, &mut doc, &char_key("a"));
            doc.commit();

            assert_eq!(
                doc.get_heads(),
                heads_before,
                "a keystroke sends bytes to the shell and mutates nothing in the shared document",
            );
            assert_eq!(
                drain_writes(&write_rx),
                b"a".to_vec(),
                "the keystroke was delivered to the shell's stdin",
            );
        }
    }

    /// The terminal's effects: every one that reaches the process or the
    /// clipboard asks for confirmation from the palette, an agent or a script.
    #[test]
    fn effects_that_reach_the_process_or_clipboard_ask_for_confirmation() {
        use ui::tilo_actions::{ConfirmationPolicy, OperationKind, PermissionRequirement};
        let specs = TerminalEffect::specs();
        assert!(specs.iter().all(|spec| spec.kind == OperationKind::Effect));
        for spec in &specs {
            let reads = matches!(
                spec.id.as_str(),
                "history-top" | "history-live" | "inspect-session" | "inspect-editor"
            );
            assert_eq!(
                spec.contract.confirmation == ConfirmationPolicy::Confirm,
                !reads,
                "`{}` confirmation",
                spec.id
            );
            let views = matches!(
                spec.id.as_str(),
                "history-top"
                    | "inspect-session"
                    | "inspect-editor"
                    | "history-live"
                    | "copy-visible"
                    | "copy-selection"
                    | "select-all"
                    | "open-link"
            );
            assert_eq!(
                spec.contract.permission == PermissionRequirement::Read,
                views,
                "`{}` permission",
                spec.id
            );
        }
    }

    /// A reviewed command presses Enter exactly once, and reviewed input may
    /// not smuggle control sequences or reach the PTY when refused.
    #[test]
    fn reviewed_input_is_bounded_and_presses_enter_exactly_once() {
        let (mut widget, writes) = build_test_widget();
        widget
            .perform(TerminalEffect::Run {
                command: "printf resource-run".into(),
            })
            .expect("run command");
        assert_eq!(drain_writes(&writes), b"printf resource-run\r".to_vec());

        for command in ["  ", "printf first\nprintf second", "printf x\x1b[200~"] {
            assert!(
                widget
                    .perform(TerminalEffect::Run {
                        command: command.into()
                    })
                    .is_err(),
                "unsafe command must be refused: {command:?}"
            );
        }
        for text in [
            String::new(),
            "x".repeat(crate::inner::MAX_RESOURCE_SEND_BYTES + 1),
            "one\ntwo".into(),
            "one\rtwo".into(),
            "one\x1b[31m".into(),
        ] {
            assert!(
                widget.perform(TerminalEffect::Send { text }).is_err(),
                "unreviewable text must be refused"
            );
        }
        assert!(
            drain_writes(&writes).is_empty(),
            "a refused input never writes a partial PTY sequence"
        );
    }

    /// Only public web links the process printed are offered as rows.
    #[test]
    fn only_public_web_osc_links_are_offered_to_agents() {
        let (mut widget, _writes) = build_test_widget();
        widget.feed(
            b"\x1b]8;;file:///home/me/.ssh/id_ed25519\x1b\\local\x1b]8;;\x1b\\ \
              \x1b]8;;https://token@example.test/private\x1b\\credential\x1b]8;;\x1b\\ \
              \x1b]8;;https://example.test/docs\x1b\\public\x1b]8;;\x1b\\",
        );
        let rows: Vec<_> = widget
            .link_rows()
            .into_iter()
            .map(ui::framework::LiveRow::into_resource)
            .collect();
        assert_eq!(
            rows.len(),
            1,
            "local and credential-bearing links are not rows"
        );
        assert_eq!(rows[0].label, "https://example.test/docs");
        assert!(
            !crate::inner::is_public_web_link("file:///home/me/.ssh/id_ed25519")
                && !crate::inner::is_public_web_link("https://token@example.test/private")
                && !crate::inner::is_public_web_link("https://example.test/?access_token=no"),
            "local paths, credentials and query-bearing targets are not public links"
        );
    }

    /// A failed or poisoned PTY writer fails the effect, and the process's
    /// input is no longer offered.
    #[test]
    fn pty_write_failures_disconnect_and_never_report_completion() {
        struct FailingWriter;
        impl Write for FailingWriter {
            fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("injected PTY write failure"))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Err(std::io::Error::other("injected PTY flush failure"))
            }
        }

        let (_out_tx, out_rx) = std::sync::mpsc::channel();
        let mut widget = TerminalWidget::new(
            Arc::new(Mutex::new(Box::new(FailingWriter))),
            Arc::new(Mutex::new(Box::new(MockMaster))),
            out_rx,
            tilo_content::ContentStore::memory(),
        );
        assert!(
            widget
                .perform(TerminalEffect::Send {
                    text: "printf should-not-run".into()
                })
                .is_err()
        );
        assert!(widget.is_disconnected());
        assert!(
            Widget::accessibility(&widget)
                .and_then(|node| node.description)
                .is_some_and(|description| description.contains("Terminal input failed"))
        );
        let unavailable: Vec<_> = widget
            .effect_statuses()
            .into_iter()
            .map(|status| format!("{status:?}"))
            .collect();
        for id in [
            "send",
            "run",
            "interrupt",
            "clear",
            "paste",
            "paste-primary",
        ] {
            assert!(
                unavailable
                    .iter()
                    .any(|status| status.contains(&format!("\"{id}\""))),
                "`{id}` is unavailable once the process's input failed: {unavailable:?}"
            );
        }
        // Moving the session elsewhere is what an ended session is for.
        assert!(
            !unavailable
                .iter()
                .any(|status| status.contains("\"open-in\"")),
            "{unavailable:?}"
        );

        let writer: Arc<Mutex<Box<dyn Write + Send>>> =
            Arc::new(Mutex::new(Box::new(std::io::sink())));
        let poison = Arc::clone(&writer);
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = poison.lock().expect("writer lock before injected panic");
            panic!("inject poisoned PTY writer");
        }));
        let (_out_tx, out_rx) = std::sync::mpsc::channel();
        let mut widget = TerminalWidget::new(
            writer,
            Arc::new(Mutex::new(Box::new(MockMaster))),
            out_rx,
            tilo_content::ContentStore::memory(),
        );
        assert!(widget.perform(TerminalEffect::Interrupt).is_err());
        assert!(widget.is_disconnected());
    }
}
