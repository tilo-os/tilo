//! Resource + action-model status for the terminal.
//!
//! The Tilo action-model recipe (`docs/action-model-recipe.md`) splits every
//! interaction into exactly one of two buckets: a *document-mutating* operation
//! becomes an `Action` (`tilo_actions::Action`) — a serializable value that
//! deterministically changes the shared Automerge document, so undo and
//! multi-peer replay come for free — while presentation / view state stays plain
//! app code. PTY input is therefore exported as an effect on the live terminal
//! resource, without fabricating document actions or a saved-session model.
//!
//! ## Everything that changes the world is external
//!
//! The terminal is a projection of an external process: a real shell running
//! behind a PTY. Spawning that shell, sending keystrokes to its stdin, running
//! commands, and the emulator parsing the bytes it prints back are all **OS /
//! driver** side effects. A keystroke travels straight to the shell — the
//! production `Widget::handle_event` keyboard path maps the key to bytes and
//! hands them to [`write_input`](crate::TerminalWidget::write_input), which owns
//! only the PTY writer and takes **no** `AutoCommit`; it can no more mutate the
//! shared document than the shell process can. By the recipe's rule none of this
//! is an action, and all of it stays as-is.
//!
//! ## The shared document is the live screen, not authored records
//!
//! What lives in the Automerge document is exactly the emulator's projection of
//! the running program's output, seeded by
//! `init_crdt` and filled by
//! [`feed`](crate::TerminalWidget::feed) / the PTY-drain `pump`:
//!
//! - `terminal_grid` and `terminal_grid_alt` — the main and alternate screen
//!   buffers, a `List` of line `Map`s each holding a `Text` and packed `attrs`;
//! - `cursor` — the cursor's `x` / `y`;
//! - `terminal_links` — OSC 8 hyperlink targets the program emitted;
//! - `terminal_images` — content hashes and cell placements for terminal
//!   graphics; immutable bytes live in the app's content store;
//! - `terminal_title` — the OSC 0/2 window title the program set;
//! - `terminal_pane_id` — a shared id that scopes collaboration *presence* (whose
//!   caret is whose), not user data.
//!
//! Every one of these is written by the terminal emulator *from the process's
//! output stream*, never by a user editing a document. It is mirrored into the
//! CRDT for one reason: so a collaborator invited to the pane sees the same live
//! screen. The CRDT already replicates that projection and the compositor renders
//! it; there is no user-authored edit to attribute, order, or undo, so
//! re-encoding a byte of program output as an `Action` would be meaningless — the
//! recipe's rule is *does a **user** operation mutate the shared document*, and
//! here the mutations are the transcript of an external process.
//!
//! ## No profiles, themes, sessions or split layouts
//!
//! The one thing that *would* be an action — a durable, user-authored record such
//! as a saved profile, a colour theme, a named session, or a persisted split
//! layout — the terminal simply does not have: it writes none of these to the
//! document (or anywhere else). Colours come from the OS-controlled `Theme` passed
//! to `draw`; there is no per-app settings map, no saved-sessions collection. The
//! generic `ui::persist` autosave in `main.rs` snapshots the whole document, but
//! that document *is* the transient screen above — it is the toolkit's screen
//! persistence, identical in every app, not a terminal-specific record model.
//!
//! ## Presentation is never an action
//!
//! The scrollback offset, the current text selection, local find-in-scrollback
//! query/matches, whether the context menu is
//! open, and the viewport are per-peer view state held on the widget. The recipe
//! names "the current selection, an overlay being open" as presentation that must
//! *never* be an action.
//!
//! ## Consequence
//!
//! The terminal publishes its live session with send/interrupt/clear effects;
//! [`take_committed_action`](ui::undo::CommittedActions::take_committed_action) stays
//! `None` (both toolkit defaults, so no [`ui::ActionHost`] is embedded — there is
//! nothing for it to dispatch, and `main.rs` registers no action decoder).
//! The bounded resource tree still exposes the live session, its transcript and
//! visible OSC links to search, agents and invocation history. Those operations
//! are typed effects/views: they drive the PTY, clipboard, viewport or URL
//! opener, but never pretend transient process state is a durable document edit.
//! The tests below prove each half of this split on the real production path.

use ui::tilo_actions::ActionRegistry;

/// Install the terminal session action decoder on the live launch path.
pub fn register(registry: &mut ActionRegistry) {
    <crate::resource::Terminal as ui::tilo_resource::Resource>::register(registry);
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::sync::{Arc, Mutex};
    use tilo_automerge::ApplyIncoming;

    use automerge::{AutoCommit, ObjType, ROOT, ReadDoc, Value};
    use portable_pty::{MasterPty, PtySize};
    use std::sync::mpsc::Receiver;
    use taffy::{NodeId, TaffyTree};
    use winit::event::{DeviceId, ElementState, KeyEvent, Modifiers, MouseButton, WindowEvent};
    use winit::keyboard::{Key, KeyLocation, NativeKeyCode, PhysicalKey, SmolStr};

    use ui::widget::Widget;

    use crate::TerminalWidget;

    /// Where the session runs and the environments it can run in: still
    /// available once its process has ended.
    const TARGET_ACTIONS: [&str; 4] = [
        "open-in",
        "create-environment",
        "grant-folder",
        "rename-environment",
    ];

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

    fn device_id() -> DeviceId {
        // winit exposes no public DeviceId constructor; tests only need a value.
        unsafe { std::mem::zeroed() }
    }

    /// A character keypress through the real winit event path (the same trick the
    /// widget's own tests use for winit's `pub(crate)` `KeyEventExtra`). Real
    /// winit delivers the committed `text` alongside a character key, which the
    /// terminal encodes into the PTY bytes.
    fn char_event(c: &str) -> WindowEvent {
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = Key::Character(SmolStr::new(c));
        ev.physical_key = PhysicalKey::Unidentified(NativeKeyCode::Unidentified);
        ev.location = KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        ev.text = Some(SmolStr::new(c));
        WindowEvent::KeyboardInput {
            device_id: device_id(),
            event: ev,
            is_synthetic: false,
        }
    }

    fn left_press() -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: device_id(),
            state: ElementState::Pressed,
            button: MouseButton::Left,
        }
    }

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
                state: doc,
                path: &ROOT,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: NodeId::new(0),
                taffy: &taffy,
            },
        );
    }

    /// Read the concatenated text of every line in a grid `List`.
    fn grid_text(doc: &AutoCommit, key: &str) -> String {
        let grid = match doc.get(&ROOT, key).unwrap() {
            Some((Value::Object(ObjType::List), id)) => id,
            other => panic!("root[\"{key}\"] must be an Automerge List, got {other:?}"),
        };
        let mut out = String::new();
        for i in 0..doc.length(&grid) {
            let line = doc.get(&grid, i).unwrap().unwrap().1;
            let text = doc.get(&line, "text").unwrap().unwrap().1;
            out.push_str(&doc.text(&text).unwrap());
        }
        out
    }

    /// The shared document is only the screen projection, keystrokes reach the shell, and output replicates byte-identically.
    #[test]
    fn document_is_the_replicated_screen_projection_and_keys_go_to_the_shell() {
        // Case `document_holds_only_the_live_screen_projection`.
        // The shared document is exactly the emulator's screen projection — the two
        // grid buffers, the cursor, the OSC hyperlink map and the presence pane id —
        // seeded by the real `init_crdt`. There is no user-authored record
        // collection anywhere: no profiles, themes, sessions, split layouts, snippets
        // or settings map. So there is nothing structural for an action to mutate;
        // every key present is written from the running program's output, not by a
        // user editing a document.
        {
            let (mut widget, _rx) = build_test_widget();
            let mut doc = AutoCommit::new();
            widget.init_crdt(&mut doc, &ROOT);

            // Exactly the live-screen keys the emulator seeds — nothing structural.
            let mut root_keys: Vec<String> = doc.keys(&ROOT).collect();
            root_keys.sort();
            assert_eq!(
                root_keys,
                vec![
                    "cursor".to_string(),
                    "terminal_grid".to_string(),
                    "terminal_grid_alt".to_string(),
                    "terminal_images".to_string(),
                    "terminal_links".to_string(),
                    "terminal_pane_id".to_string(),
                ],
                "the document holds only the emulator's live-screen projection: {root_keys:?}",
            );

            // Both grids are screen buffers; cursor is a coord map; links is the OSC 8
            // map — every key is process output, never an authored record.
            for (key, want) in [
                ("terminal_grid", ObjType::List),
                ("terminal_grid_alt", ObjType::List),
                ("cursor", ObjType::Map),
                ("terminal_links", ObjType::Map),
            ] {
                match doc.get(&ROOT, key).unwrap() {
                    Some((Value::Object(got), _)) if got == want => {}
                    other => panic!("root[\"{key}\"] must be a {want:?}, got {other:?}"),
                }
            }

            // No durable, user-authored record collection exists to hang an action
            // off — proving this isn't a "saved profiles / sessions are actions" case.
            for forbidden in [
                "profiles",
                "profile",
                "themes",
                "theme",
                "sessions",
                "session",
                "layouts",
                "splits",
                "snippets",
                "favourites",
                "favorites",
                "settings",
                "prefs",
                "bookmarks",
            ] {
                assert!(
                    doc.get(&ROOT, forbidden).unwrap().is_none(),
                    "document must not carry a `{forbidden}` record collection",
                );
            }
        }

        // Case `keystrokes_go_to_the_shell_not_the_document`.
        // A keystroke is **external**: driven through the production
        // `Widget::handle_event`, the character reaches the shell's stdin (arrives on
        // the PTY-write channel) while the shared document's heads are **unchanged**
        // — the input path holds only the PTY writer and takes no `AutoCommit`, so it
        // writes nothing to the CRDT. Hence typing is not an action.
        {
            let (mut widget, write_rx) = build_test_widget();
            let mut doc = AutoCommit::new();

            // Seed the screen projection once (the real path), then settle — after
            // this `init_crdt` is doc-idempotent, so any later change is the keystroke.
            widget.init_crdt(&mut doc, &ROOT);
            doc.commit();
            let heads_before = doc.get_heads();
            let _ = drain_writes(&write_rx); // ignore anything queued during seeding

            // Press 'a' through the production keyboard path.
            send(&mut widget, &mut doc, &char_event("a"));
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

        // Case `pty_output_projects_into_the_grid_and_replicates_byte_identically`.
        // The collaboration guarantee the toolkit's CRDT already provides for the
        // screen projection — the same one an action executor would give, and the
        // reason no action is needed: the *external* program's output, fed through
        // the production `feed`, fills `terminal_grid`, and an independent replica
        // that merges the change converges **byte-identically** (same head hashes,
        // not merely the same visible text).
        {
            let (mut widget, _rx) = build_test_widget();
            let mut doc = AutoCommit::new();
            widget.init_crdt(&mut doc, &ROOT);
            doc.commit();

            let base = doc.save();
            let mut peer = AutoCommit::load(&base).unwrap();
            assert_eq!(
                doc.get_heads(),
                peer.get_heads(),
                "precondition: replicas share a base"
            );

            // The shell prints "hello" — an external side effect the emulator projects
            // into the grid. `feed` is the exact path the live PTY-drain `pump` runs.
            widget.feed(&mut doc, &ROOT, b"hello");
            doc.commit();
            assert!(
                grid_text(&doc, "terminal_grid").contains("hello"),
                "the program's output landed in the screen buffer",
            );

            // Merge into the peer: byte-identical convergence, for free, no action.
            peer.merge_incoming(&mut doc).unwrap();
            assert_eq!(
                doc.get_heads(),
                peer.get_heads(),
                "byte-identical convergence after merge"
            );
            assert!(
                grid_text(&peer, "terminal_grid").contains("hello"),
                "the collaborator sees the same live screen",
            );
        }
    }

    /// Exported terminal effects: no document undo boundary, bounded commands and payloads, and only public web links.
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "cases merged from separate tests; each case stays self-contained"
    )]
    fn exported_effects_are_bounded_and_carry_no_document_undo() {
        // Case `exports_terminal_effects_without_document_actions`.
        // The live session exports PTY effects, but none creates a document undo
        // boundary.
        {
            let (mut widget, _rx) = build_test_widget();
            let mut doc = AutoCommit::new();
            widget.init_crdt(&mut doc, &ROOT);

            // Real event/output path: program prints, user types, user clicks.
            widget.feed(&mut doc, &ROOT, b"hello");
            send(&mut widget, &mut doc, &char_event("x"));
            send(&mut widget, &mut doc, &left_press());

            let resources = ui::resources::resources(&widget, &doc);
            assert_eq!(resources.len(), 1, "one terminal-session root");
            let terminal = &resources[0];
            assert_eq!(terminal.kind, "terminal.session");
            // publish() always offers apply/group for document session actions;
            // PTY controls remain Effects on the same resource.
            let effect_ids: Vec<_> = terminal
                .actions
                .iter()
                .filter(|action| action.kind == ui::tilo_actions::OperationKind::Effect)
                .map(|action| action.id.as_str())
                .collect();
            assert_eq!(
                effect_ids,
                vec![
                    "send",
                    "run",
                    "open-in",
                    "create-environment",
                    "grant-folder",
                    "rename-environment",
                    "interrupt",
                    "clear",
                    "paste",
                    "paste-primary"
                ]
            );
            assert!(terminal.actions.iter().any(|action| action.id == "apply"
                && action.kind == ui::tilo_actions::OperationKind::Document));
            assert!(
                ui::undo::take_committed_action(&mut widget).is_none(),
                "no action is dispatched, so no action-host undo boundary is recorded",
            );
            assert!(
                terminal
                    .actions
                    .iter()
                    .filter(|action| action.kind == ui::tilo_actions::OperationKind::Effect)
                    .all(|action| {
                        matches!(
                            action.contract.confirmation,
                            ui::tilo_actions::ConfirmationPolicy::Confirm
                        ) && matches!(
                            action.contract.risk,
                            ui::tilo_actions::RiskLevel::Privileged
                        ) && matches!(
                            action.contract.retry,
                            ui::tilo_actions::RetryPolicy::AtMostOnce
                        )
                    }),
                "palette and agents must explicitly confirm every operation that can affect a live shell"
            );
            let transcript = terminal
                .children
                .iter()
                .find(|resource| resource.kind == "terminal.transcript")
                .expect("session owns a canonical transcript resource");
            assert_eq!(
                transcript
                    .actions
                    .iter()
                    .map(|action| action.id.as_str())
                    .collect::<Vec<_>>(),
                vec![
                    "history-top",
                    "history-live",
                    "copy-visible",
                    "copy-selection",
                    "select-all"
                ]
            );
            assert!(
                transcript
                    .actions
                    .iter()
                    .filter(|action| action.kind == ui::tilo_actions::OperationKind::Effect)
                    .all(|action| {
                        action.contract.confirmation
                            == ui::tilo_actions::ConfirmationPolicy::Confirm
                            && action.contract.risk == ui::tilo_actions::RiskLevel::Privileged
                            && action.contract.retry == ui::tilo_actions::RetryPolicy::AtMostOnce
                    }),
                "clipboard and selection-publication effects retain explicit admission contracts"
            );
        }

        // Case `run_command_is_bounded_and_presses_enter_exactly_once`.
        {
            let (mut widget, writes) = build_test_widget();
            let mut doc = AutoCommit::new();
            let payload = serde_json::to_vec("printf resource-run").expect("serialize command");
            assert!(
                Widget::start_operation(
                    &mut widget,
                    &ui::widget::ResourceOperation::local(
                        ui::tilo_actions::InvocationId::new(),
                        ui::tilo_actions::OperationKind::Effect,
                        "terminal.run",
                        &payload
                    ),
                    &mut doc,
                    &ROOT
                )
                .map(|started| started.is_some())
                .expect("invoke run command")
            );
            assert_eq!(drain_writes(&writes), b"printf resource-run\r".to_vec());

            let blank = serde_json::to_vec("  ").expect("serialize blank command");
            assert!(
                Widget::start_operation(
                    &mut widget,
                    &ui::widget::ResourceOperation::local(
                        ui::tilo_actions::InvocationId::new(),
                        ui::tilo_actions::OperationKind::Effect,
                        "terminal.run",
                        &blank
                    ),
                    &mut doc,
                    &ROOT
                )
                .map(|started| started.is_some())
                .is_err()
            );

            // A reviewed `run` action must be exactly one command. Newlines or
            // escape bytes would turn the promised single trailing Enter into an
            // arbitrary sequence of shell submissions/control operations.
            for dangerous in ["printf first\nprintf second", "printf x\x1b[200~"] {
                let payload = serde_json::to_vec(dangerous).expect("serialize control payload");
                assert!(
                    Widget::start_operation(
                        &mut widget,
                        &ui::widget::ResourceOperation::local(
                            ui::tilo_actions::InvocationId::new(),
                            ui::tilo_actions::OperationKind::Effect,
                            "terminal.run",
                            &payload
                        ),
                        &mut doc,
                        &ROOT
                    )
                    .map(|started| started.is_some())
                    .is_err(),
                    "unsafe multi-effect command must be rejected: {dangerous:?}"
                );
            }
            assert!(
                drain_writes(&writes).is_empty(),
                "a rejected reviewed command must not write a partial PTY sequence"
            );
        }

        // Case `resource_send_refuses_empty_or_unreviewably_large_payloads`.
        {
            let (mut widget, writes) = build_test_widget();
            let mut doc = AutoCommit::new();
            let root = ROOT;
            let empty = serde_json::to_vec("").expect("serialize empty string");
            assert!(
                Widget::start_operation(
                    &mut widget,
                    &ui::widget::ResourceOperation::local(
                        ui::tilo_actions::InvocationId::new(),
                        ui::tilo_actions::OperationKind::Effect,
                        "terminal.send",
                        &empty
                    ),
                    &mut doc,
                    &root
                )
                .map(|started| started.is_some())
                .is_err()
            );
            let too_large =
                serde_json::to_vec(&"x".repeat(crate::inner::MAX_RESOURCE_SEND_BYTES + 1))
                    .expect("serialize large command");
            assert!(
                Widget::start_operation(
                    &mut widget,
                    &ui::widget::ResourceOperation::local(
                        ui::tilo_actions::InvocationId::new(),
                        ui::tilo_actions::OperationKind::Effect,
                        "terminal.send",
                        &too_large
                    ),
                    &mut doc,
                    &root
                )
                .map(|started| started.is_some())
                .is_err()
            );
            assert!(
                writes.try_recv().is_err(),
                "rejected input never reaches the PTY"
            );

            for dangerous in ["one\ntwo", "one\rtwo", "one\x1b[31m"] {
                let payload = serde_json::to_vec(dangerous).expect("serialize control payload");
                assert!(
                    Widget::start_operation(
                        &mut widget,
                        &ui::widget::ResourceOperation::local(
                            ui::tilo_actions::InvocationId::new(),
                            ui::tilo_actions::OperationKind::Effect,
                            "terminal.send",
                            &payload
                        ),
                        &mut doc,
                        &root
                    )
                    .map(|started| started.is_some())
                    .is_err(),
                    "send text must not smuggle a terminal control sequence: {dangerous:?}"
                );
            }
        }

        // Case `only_public_web_osc_links_are_exported_to_agents`.
        {
            let (mut widget, _writes) = build_test_widget();
            let mut doc = AutoCommit::new();
            widget.init_crdt(&mut doc, &ROOT);
            widget.feed(
                &mut doc,
                &ROOT,
                b"\x1b]8;;file:///home/me/.ssh/id_ed25519\x1b\\local\x1b]8;;\x1b\\ \
              \x1b]8;;https://token@example.test/private\x1b\\credential\x1b]8;;\x1b\\ \
              \x1b]8;;https://example.test/docs\x1b\\public\x1b]8;;\x1b\\",
            );
            widget.sync(&mut doc, &ROOT);

            let transcript = ui::resources::resources(&widget, &doc)
                .into_iter()
                .next()
                .expect("session resource")
                .children
                .into_iter()
                .find(|resource| resource.kind == "terminal.transcript")
                .expect("transcript resource");
            assert_eq!(
                transcript.children.len(),
                1,
                "local and credential-bearing process links are not agent resources"
            );
            assert_eq!(transcript.children[0].label, "https://example.test/docs");
            assert!(
                !crate::inner::is_public_web_link("file:///home/me/.ssh/id_ed25519")
                    && !crate::inner::is_public_web_link("https://token@example.test/private")
                    && !crate::inner::is_public_web_link("https://example.test/?access_token=no"),
                "the directory rejects local paths, credentials, and query-bearing targets"
            );
        }
    }

    /// A failed or poisoned PTY writer fails the operation, publishes disconnected, and never reports completion.
    #[test]
    fn pty_write_failures_disconnect_and_never_report_completion() {
        // Case `resource_operations_fail_and_publish_disconnected_after_a_pty_write_error`.
        {
            struct FailingWriter;
            impl Write for FailingWriter {
                fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
                    Err(std::io::Error::other("injected PTY write failure"))
                }
                fn flush(&mut self) -> std::io::Result<()> {
                    Err(std::io::Error::other("injected PTY flush failure"))
                }
            }

            let writer: Arc<Mutex<Box<dyn Write + Send>>> =
                Arc::new(Mutex::new(Box::new(FailingWriter)));
            let (_out_tx, out_rx) = std::sync::mpsc::channel();
            let mut widget = TerminalWidget::new(
                writer,
                Arc::new(Mutex::new(Box::new(MockMaster))),
                out_rx,
                tilo_content::ContentStore::memory(),
            );
            let mut doc = AutoCommit::new();
            let payload = serde_json::to_vec("printf should-not-run").expect("serialize command");
            assert!(
                Widget::start_operation(
                    &mut widget,
                    &ui::widget::ResourceOperation::local(
                        ui::tilo_actions::InvocationId::new(),
                        ui::tilo_actions::OperationKind::Effect,
                        "terminal.send",
                        &payload
                    ),
                    &mut doc,
                    &ROOT
                )
                .map(|started| started.is_some())
                .is_err()
            );
            assert!(widget.is_disconnected());
            assert!(
                Widget::accessibility(&widget)
                    .and_then(|node| node.description)
                    .is_some_and(|description| description.contains("Terminal input failed"))
            );
            assert!(
                ui::resources::resources(&widget, &doc)
                    .into_iter()
                    .all(|resource| {
                        resource
                            .actions
                            .iter()
                            .filter(|action| action.kind == ui::tilo_actions::OperationKind::Effect)
                            // Moving the session elsewhere is what an ended
                            // session is for; only its process input closes.
                            .filter(|action| !TARGET_ACTIONS.contains(&action.id.as_str()))
                            .all(|action| {
                                matches!(
                                    action.contract.availability,
                                    ui::tilo_actions::Availability::Unavailable { .. }
                                )
                            })
                    })
            );
        }

        // Case `poisoned_pty_writer_never_reports_a_completed_control_effect`.
        {
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
            let mut doc = AutoCommit::new();
            let payload = serde_json::to_vec("interrupt").expect("serialize control");
            assert!(
                Widget::start_operation(
                    &mut widget,
                    &ui::widget::ResourceOperation::local(
                        ui::tilo_actions::InvocationId::new(),
                        ui::tilo_actions::OperationKind::Effect,
                        "terminal.control",
                        &payload
                    ),
                    &mut doc,
                    &ROOT
                )
                .map(|started| started.is_some())
                .is_err()
            );
            assert!(widget.is_disconnected());
        }
    }

    /// A live session that printed a titled prompt with a public OSC 8 link,
    /// projected into the document through the real emulator path.
    fn live_session() -> (TerminalWidget, AutoCommit) {
        let (mut widget, _writes) = build_test_widget();
        let mut doc = AutoCommit::new();
        widget.init_crdt(&mut doc, &ROOT);
        widget.feed(
            &mut doc,
            &ROOT,
            b"\x1b]2;build\x07$ make\r\nsee \x1b]8;;https://example.test/docs\x1b\\docs\x1b]8;;\x1b\\\r\n",
        );
        widget.sync(&mut doc, &ROOT);
        doc.commit();
        (widget, doc)
    }

    // The terminal publishes only effects and views, so the sweep checks the
    // surface is non-empty and protocol-valid with an empty registry.
    ui::action_contract_tests! {
        seed: live_session().1,
        registry: ui::tilo_actions::ActionRegistry::new(),
        resources: |doc| ui::resources::resources(&live_session().0, doc),
        palette: [
            "Send text",
            "Run command",
            "Interrupt process",
            "Clear screen",
            "Copy visible output",
        ],
    }
}
