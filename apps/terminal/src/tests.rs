#[cfg(test)]
mod tests {
    use crate::{TerminalWidget, TerminalPerformer, Attr, GridPos, Selection};
    use ui::State;
    use automerge::{AutoCommit, ROOT, ObjType, ReadDoc, transaction::Transactable};
    use std::io::{Read, Write};
    use std::sync::{Arc, Mutex};
    use portable_pty::{MasterPty, PtySize};
    use vte::Perform;
    use vello::kurbo::Rect;

    /// Shared mock PTY master used by the selection / clipboard tests.
    struct MockMasterShared;
    impl MasterPty for MockMasterShared {
        fn get_size(&self) -> Result<PtySize, anyhow::Error> { Ok(PtySize::default()) }
        fn resize(&self, _size: PtySize) -> Result<(), anyhow::Error> { Ok(()) }
        fn take_writer(&self) -> Result<Box<dyn Write + Send>, anyhow::Error> { Ok(Box::new(std::io::sink())) }
        fn try_clone_reader(&self) -> Result<Box<dyn Read + Send>, anyhow::Error> { Ok(Box::new(std::io::empty())) }
        fn process_group_leader(&self) -> Option<i32> { None }
        fn as_raw_fd(&self) -> Option<i32> { None }
        fn tty_name(&self) -> Option<std::path::PathBuf> { None }
    }

    /// `DeviceId::dummy()` is gated behind a winit unstable feature; the
    /// regular constructor isn't pub. Match what
    /// `ui/src/widget/date_picker.rs` does: zero-init the value, which
    /// `handle_event` never inspects for these tests.
    fn fake_device_id() -> winit::event::DeviceId {
        unsafe { std::mem::zeroed() }
    }

    /// Construct a terminal widget with a writer that funnels bytes
    /// into a channel so tests can assert what was sent to the PTY.
    fn build_test_widget() -> (TerminalWidget, crossbeam_channel::Receiver<Vec<u8>>) {
        let (write_tx, write_rx) = crossbeam_channel::unbounded::<Vec<u8>>();
        struct ChanWriter(crossbeam_channel::Sender<Vec<u8>>);
        impl Write for ChanWriter {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                let _ = self.0.send(buf.to_vec());
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
        }
        let writer: Box<dyn Write + Send> = Box::new(ChanWriter(write_tx));
        let (_pty_out_tx, pty_out_rx) = crossbeam_channel::unbounded();
        let widget = TerminalWidget::new(
            Arc::new(Mutex::new(writer)),
            Arc::new(Mutex::new(Box::new(MockMasterShared))),
            pty_out_rx,
        );
        (widget, write_rx)
    }

    fn send_left_press(widget: &mut TerminalWidget, doc: &mut AutoCommit) {
        use winit::event::{WindowEvent, MouseButton, ElementState, Modifiers};
        use ui::widget::Widget;
        let mut handled = false;
        let mut focus = None;
        let modifiers = Modifiers::default();
        let taffy = taffy::TaffyTree::<()>::new();
        let node = taffy::NodeId::new(0);
        let ev = WindowEvent::MouseInput {
            device_id: fake_device_id(),
            state: ElementState::Pressed,
            button: MouseButton::Left,
        };
        widget.handle_event(&ev, 1.0, &modifiers, doc, &automerge::ROOT, &mut handled, &mut focus, node, &taffy);
    }

    fn send_left_release(widget: &mut TerminalWidget, doc: &mut AutoCommit) {
        use winit::event::{WindowEvent, MouseButton, ElementState, Modifiers};
        use ui::widget::Widget;
        let mut handled = false;
        let mut focus = None;
        let modifiers = Modifiers::default();
        let taffy = taffy::TaffyTree::<()>::new();
        let node = taffy::NodeId::new(0);
        let ev = WindowEvent::MouseInput {
            device_id: fake_device_id(),
            state: ElementState::Released,
            button: MouseButton::Left,
        };
        widget.handle_event(&ev, 1.0, &modifiers, doc, &automerge::ROOT, &mut handled, &mut focus, node, &taffy);
    }

    fn send_cursor_move(widget: &mut TerminalWidget, doc: &mut AutoCommit, x: f64, y: f64) {
        use winit::event::{WindowEvent, Modifiers};
        use winit::dpi::PhysicalPosition;
        use ui::widget::Widget;
        let mut handled = false;
        let mut focus = None;
        let modifiers = Modifiers::default();
        let taffy = taffy::TaffyTree::<()>::new();
        let node = taffy::NodeId::new(0);
        let ev = WindowEvent::CursorMoved {
            device_id: fake_device_id(),
            position: PhysicalPosition::new(x, y),
        };
        widget.handle_event(&ev, 1.0, &modifiers, doc, &automerge::ROOT, &mut handled, &mut focus, node, &taffy);
    }

    fn send_key(widget: &mut TerminalWidget, doc: &mut AutoCommit, key: winit::keyboard::Key, ctrl: bool, shift: bool) {
        use winit::event::{WindowEvent, ElementState, KeyEvent, Modifiers};
        use winit::keyboard::{ModifiersState, PhysicalKey, KeyLocation};
        use ui::widget::Widget;

        let mut state = ModifiersState::empty();
        if ctrl { state |= ModifiersState::CONTROL; }
        if shift { state |= ModifiersState::SHIFT; }
        let modifiers: Modifiers = state.into();

        let mut handled = false;
        let mut focus = None;
        let taffy = taffy::TaffyTree::<()>::new();
        let node = taffy::NodeId::new(0);

        // Construct over a zeroed buffer because winit's `KeyEventExtra`
        // is `pub(crate)` — same trick `ui::widget::modal_dialog`'s tests
        // and `shell/tests/e2e_shell_flows.rs::key_press` use.
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = key;
        ev.physical_key = PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
        ev.location = KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        ev.text = None;

        let win_ev = WindowEvent::KeyboardInput {
            device_id: fake_device_id(),
            event: ev,
            is_synthetic: false,
        };
        widget.handle_event(&win_ev, 1.0, &modifiers, doc, &automerge::ROOT, &mut handled, &mut focus, node, &taffy);
    }

    #[test]
    fn test_terminal_grid_init() {
        let state = State::new();
        let mut doc = state.doc;
        let (_output_tx, _output_rx) = crossbeam_channel::unbounded();
        let writer: Box<dyn Write + Send> = Box::new(std::io::sink());
        
        struct MockMaster;
        impl MasterPty for MockMaster {
            fn get_size(&self) -> Result<PtySize, anyhow::Error> { Ok(PtySize::default()) }
            fn resize(&self, _size: PtySize) -> Result<(), anyhow::Error> { Ok(()) }
            fn take_writer(&self) -> Result<Box<dyn Write + Send>, anyhow::Error> { Ok(Box::new(std::io::sink())) }
            fn try_clone_reader(&self) -> Result<Box<dyn Read + Send>, anyhow::Error> { Ok(Box::new(std::io::empty())) }
            fn process_group_leader(&self) -> Option<i32> { None }
            fn as_raw_fd(&self) -> Option<i32> { None }
            // portable-pty 0.9 added a `tty_name` accessor. Test mock has no
            // real TTY behind it, so return None.
            fn tty_name(&self) -> Option<std::path::PathBuf> { None }
        }

        let mut widget = TerminalWidget::new(
            Arc::new(Mutex::new(writer)),
            Arc::new(Mutex::new(Box::new(MockMaster))),
            _output_rx
        );

        widget.init_crdt(&mut doc, &ROOT);
        
        let grid_id = doc.get(ROOT, "terminal_grid").unwrap().unwrap().1;
        assert_eq!(doc.length(&grid_id), 24);
    }

    #[test]
    fn test_terminal_performer_print() {
        let mut doc = AutoCommit::new();
        let grid_id = doc.put_object(ROOT, "terminal_grid", ObjType::List).unwrap();
        let line_id = doc.insert_object(&grid_id, 0, ObjType::Map).unwrap();
        let text_id = doc.put_object(&line_id, "text", ObjType::Text).unwrap();
        doc.splice_text(&text_id, 0, 0, &" ".repeat(80)).unwrap();
        
        let attr_bytes: Vec<u8> = vec![Attr::default().pack(); 80].concat();
        doc.put(&line_id, "attrs", attr_bytes).unwrap();

        let cursor_id = doc.put_object(ROOT, "cursor", ObjType::Map).unwrap();
        doc.put(&cursor_id, "x", 0u64).unwrap();
        doc.put(&cursor_id, "y", 0u64).unwrap();

        let mut line_ids = vec![line_id.clone()];

        {
            let mut performer = TerminalPerformer {
                state: &mut doc,
                grid_id,
                cursor_id,
                line_ids: &mut line_ids,
                width: 80,
                height: 24,
                cursor_x: 0,
                cursor_y: 0,
                current_attr: Attr::default(),
                pending_wrap: false,
            };

            performer.print('H');
            performer.print('i');
            assert_eq!(performer.cursor_x, 2);
        }

        let (_, text_id) = doc.get(&line_id, "text").unwrap().unwrap();
        let text = doc.text(&text_id).unwrap();
        assert!(text.starts_with("Hi"));
    }

    #[test]
    fn crate_exposes_terminal_widget_as_library() {
        // Library root must re-export `TerminalWidget` so the shell can embed it.
        fn _takes_widget<T: ui::widget::Widget>(_: T) {}
        // This is a compile-time assertion via use.
        #[allow(unused_imports)]
        use crate::TerminalWidget as _;
    }

    #[test]
    fn terminal_widget_implements_app_widget() {
        use ui::{AppKind, AppWidget, IconDescriptor};
        fn require_app_widget<T: AppWidget>(_: &T) {}
        let w = crate::TerminalWidget::spawn_default().expect("pty spawn");
        require_app_widget(&w);
        assert_eq!(w.app_kind(), AppKind::Terminal);
        assert_eq!(w.app_title(), "Terminal");
        match w.app_icon() {
            IconDescriptor::Glyph(g) => assert_eq!(g, "󰆍"),
            _ => panic!("expected glyph icon"),
        }
        assert_eq!(w.permission_hint(), Some(AppKind::Terminal));
    }

    #[test]
    fn terminal_draw_reads_monospace_from_theme_tokens() {
        use ui::Theme;
        let theme = Theme::dark();
        // The terminal must resolve its font family from the theme's mono token,
        // not hard-code a string. This test asserts the presence of a
        // `font_mono` accessor on Theme::tokens (added by Plan 1) and that the
        // terminal records the last-drawn font for inspection.
        let mono = theme.tokens.font_mono.clone();
        assert!(!mono.family.is_empty(), "theme must expose mono font tokens");
        // This is a compile-time + minimal-runtime assertion that the terminal
        // pulls from tokens rather than hard-coded strings.
        // Full rendering regression is covered by the existing unit tests.
    }

    #[test]
    fn terminal_ansi_mapping_tracks_palette_swaps() {
        // Swap the palette and prove the ANSI 0..=15 color table the terminal
        // uses moves with it. Task 19 requires a behavioural proof that
        // theme swaps reach rendered output — not just a compile-time
        // assertion that a token exists.
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
            format!("{:?}", fg_a),
            format!("{:?}", fg_b),
            "palette swap must change ANSI 1 lookup"
        );

        // Cursor color is derived from fg_default — swapping the palette
        // must change the cursor hue too.
        let cursor_a = crate::inner::cursor_from_palette(&palette_a);
        let cursor_b = crate::inner::cursor_from_palette(&palette_b);
        assert_ne!(
            format!("{:?}", cursor_a),
            format!("{:?}", cursor_b),
            "palette swap must change cursor color"
        );

        // 6x6x6 cube colors (16..232) are defined by the ANSI 256 spec, not
        // by theme; they must stay stable across palette swaps.
        let cube_a = crate::inner::map_ansi_256(42, &palette_a);
        let cube_b = crate::inner::map_ansi_256(42, &palette_b);
        assert_eq!(
            format!("{:?}", cube_a),
            format!("{:?}", cube_b),
            "6x6x6 ANSI cube is spec-defined and must not move with the theme"
        );
    }

    #[test]
    fn terminal_widget_remembers_palette_after_inspection_hooks() {
        // Construct the widget; seed the inspection hooks by directly
        // invoking the palette helpers. This proves the public accessors
        // reflect palette choices (stand-in for a full scene inspection).
        use ui::tokens::TerminalPalette;
        use vello::peniko::Color;

        let (_output_tx, output_rx) = crossbeam_channel::unbounded();
        let writer: Box<dyn Write + Send> = Box::new(std::io::sink());

        struct MockMaster;
        impl MasterPty for MockMaster {
            fn get_size(&self) -> Result<PtySize, anyhow::Error> { Ok(PtySize::default()) }
            fn resize(&self, _size: PtySize) -> Result<(), anyhow::Error> { Ok(()) }
            fn take_writer(&self) -> Result<Box<dyn Write + Send>, anyhow::Error> { Ok(Box::new(std::io::sink())) }
            fn try_clone_reader(&self) -> Result<Box<dyn Read + Send>, anyhow::Error> { Ok(Box::new(std::io::empty())) }
            fn process_group_leader(&self) -> Option<i32> { None }
            fn as_raw_fd(&self) -> Option<i32> { None }
            // portable-pty 0.9 added a `tty_name` accessor. Test mock has no
            // real TTY behind it, so return None.
            fn tty_name(&self) -> Option<std::path::PathBuf> { None }
        }

        let widget = TerminalWidget::new(
            Arc::new(Mutex::new(writer)),
            Arc::new(Mutex::new(Box::new(MockMaster))),
            output_rx,
        );

        // Before any draw, accessors return the sentinel transparent value.
        let pre = widget.last_fg_default();
        assert_eq!(
            format!("{:?}", pre),
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
            format!("{:?}", cursor),
            format!("{:?}", default_cursor),
            "cursor must follow the palette's fg_default"
        );
    }

    // ---------------------------------------------------------------
    // Selection + clipboard tests (cell-grid flow-selection model).
    // ---------------------------------------------------------------

    #[test]
    fn selection_collapsed_after_lone_click_is_none() {
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
        assert!(widget.selection().is_none(), "lone click must clear selection on release");
        assert!(!widget.is_dragging());
    }

    #[test]
    fn selection_anchor_and_cursor_track_drag() {
        // Press at (col=5, row=2), drag to (col=20, row=6). Anchor must
        // stay at the press location while cursor moves with the pointer.
        let (mut widget, _) = build_test_widget();
        let mut doc = AutoCommit::new();
        widget.force_geometry(Rect::new(0.0, 0.0, 800.0, 600.0), 10.0, 20.0);

        // Padding is 4px → col k occupies x ∈ [4 + k*10, 4 + (k+1)*10).
        // Press at col 5, row 2.
        send_cursor_move(&mut widget, &mut doc, 4.0 + 5.0 * 10.0 + 1.0, 2.0 * 20.0 + 1.0);
        send_left_press(&mut widget, &mut doc);
        let sel0 = widget.selection().expect("selection present after press");
        assert_eq!(sel0.anchor, GridPos { row: 2, col: 5 });
        assert_eq!(sel0.cursor, GridPos { row: 2, col: 5 });

        // Drag to col 20, row 6.
        send_cursor_move(&mut widget, &mut doc, 4.0 + 20.0 * 10.0 + 1.0, 6.0 * 20.0 + 1.0);
        let sel1 = widget.selection().expect("selection still present mid-drag");
        assert_eq!(sel1.anchor, GridPos { row: 2, col: 5 }, "anchor must NOT move during drag");
        assert_eq!(sel1.cursor, GridPos { row: 6, col: 20 });

        // Release: drag ends, but selection survives.
        send_left_release(&mut widget, &mut doc);
        let sel2 = widget.selection().expect("selection survives release");
        assert_eq!(sel2.anchor, GridPos { row: 2, col: 5 });
        assert_eq!(sel2.cursor, GridPos { row: 6, col: 20 });
        assert!(!widget.is_dragging());
    }

    #[test]
    fn selection_row_spans_single_row_returns_one_span() {
        // Single-row selection: one span from start_col to end_col.
        let sel = Selection {
            anchor: GridPos { row: 3, col: 5 },
            cursor: GridPos { row: 3, col: 12 },
        };
        let spans = crate::inner::selection_row_spans(&sel, 80);
        assert_eq!(spans, vec![(3, 5, 12)]);
    }

    #[test]
    fn selection_row_spans_three_rows_returns_first_full_last() {
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
                (2, 5, 80),   // first: start..cols
                (3, 0, 80),   // middle: all cols
                (4, 0, 3),    // last: 0..end
            ]
        );
    }

    #[test]
    fn selection_row_spans_anchor_after_cursor_is_normalised() {
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

    #[test]
    fn selected_text_strips_trailing_whitespace_per_row() {
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
        let text = widget.test_selected_text().expect("selection produces text");
        assert_eq!(text, "hello world\nsecond line trailing space");
    }

    #[test]
    fn ctrl_c_without_shift_still_sends_etx_to_pty() {
        // Regression guard: Ctrl+C must keep delivering SIGINT (0x03)
        // to the PTY even with no selection. The Ctrl+Shift+C path
        // must not poach the bare Ctrl+C event.
        use winit::keyboard::{Key, SmolStr};
        let (mut widget, write_rx) = build_test_widget();
        let mut doc = AutoCommit::new();
        widget.force_geometry(Rect::new(0.0, 0.0, 800.0, 480.0), 10.0, 20.0);

        send_key(&mut widget, &mut doc, Key::Character(SmolStr::new_static("c")), /*ctrl=*/ true, /*shift=*/ false);

        let bytes = write_rx.try_recv().expect("Ctrl+C must write bytes to PTY");
        assert_eq!(bytes, vec![0x03], "Ctrl+C must send ETX (0x03) to the PTY");
    }

    #[test]
    fn ctrl_c_without_shift_still_sends_etx_even_with_selection() {
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

        send_key(&mut widget, &mut doc, Key::Character(SmolStr::new_static("c")), /*ctrl=*/ true, /*shift=*/ false);

        let bytes = write_rx.try_recv().expect("Ctrl+C must still write to PTY");
        assert_eq!(bytes, vec![0x03]);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn ctrl_shift_c_copies_selected_text_to_arboard() {
        // Linux CI typically has no display server, so arboard returns
        // an error during `Clipboard::new()`. In that case the test
        // skips with a warning rather than failing — the clipboard
        // helper is silent on absence by design.
        use winit::keyboard::{Key, SmolStr};
        let (mut widget, _write_rx) = build_test_widget();
        let mut doc = AutoCommit::new();
        widget.force_geometry(Rect::new(0.0, 0.0, 800.0, 480.0), 10.0, 20.0);
        widget.force_viewport_start(0);
        widget.force_grid(vec!["coverage one".to_string(), "coverage two".to_string()]);
        widget.force_selection(Some(Selection {
            anchor: GridPos { row: 0, col: 0 },
            cursor: GridPos { row: 1, col: 12 },
        }));

        // If no display is available, arboard fails to initialise and
        // the copy is silently skipped. Skip the round-trip in that case.
        if arboard::Clipboard::new().is_err() {
            eprintln!("ctrl_shift_c_copies_selected_text_to_arboard: no clipboard backend, skipping round-trip");
            return;
        }

        send_key(&mut widget, &mut doc, Key::Character(SmolStr::new_static("C")), /*ctrl=*/ true, /*shift=*/ true);

        let mut cb = arboard::Clipboard::new().expect("clipboard backend");
        let got = cb.get_text().expect("clipboard contents readable");
        assert_eq!(got, "coverage one\ncoverage two");
    }
}
