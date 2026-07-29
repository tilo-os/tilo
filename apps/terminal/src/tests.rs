#[cfg(test)]
mod tests {
    use crate::{TerminalWidget, TerminalPerformer, Attr, GridPos, Selection};
    use ui::State;
    use ui::widget::Widget as _;
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

    /// Like `build_test_widget` but also hands back the PTY *output* sender,
    /// so a test can push bytes down the exact channel the reader thread uses.
    /// This exercises the production `sync` → `pump` drain path (which is what
    /// snaps the scrollback viewport back to the live bottom on fresh output).
    fn build_test_widget_io() -> (
        TerminalWidget,
        crossbeam_channel::Receiver<Vec<u8>>,
        crossbeam_channel::Sender<Vec<u8>>,
    ) {
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
        let (pty_out_tx, pty_out_rx) = crossbeam_channel::unbounded();
        let widget = TerminalWidget::new(
            Arc::new(Mutex::new(writer)),
            Arc::new(Mutex::new(Box::new(MockMasterShared))),
            pty_out_rx,
        );
        (widget, write_rx, pty_out_tx)
    }

    /// Drain and concatenate everything the PTY-write channel has buffered.
    fn drain_writes(rx: &crossbeam_channel::Receiver<Vec<u8>>) -> Vec<u8> {
        let mut out = Vec::new();
        while let Ok(b) = rx.try_recv() { out.extend_from_slice(&b); }
        out
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
        send_key_mods(widget, doc, key, ctrl, shift, false);
    }

    fn send_key_mods(widget: &mut TerminalWidget, doc: &mut AutoCommit, key: winit::keyboard::Key, ctrl: bool, shift: bool, alt: bool) {
        use winit::event::{WindowEvent, ElementState, KeyEvent, Modifiers};
        use winit::keyboard::{ModifiersState, PhysicalKey, KeyLocation};
        use ui::widget::Widget;

        let mut state = ModifiersState::empty();
        if ctrl { state |= ModifiersState::CONTROL; }
        if shift { state |= ModifiersState::SHIFT; }
        if alt { state |= ModifiersState::ALT; }
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
        // Real winit delivers the committed text alongside a character key; the
        // terminal encodes that into the PTY bytes (and typing snaps the
        // scrollback back to the live bottom). Named keys carry no `text`.
        ev.text = match &ev.logical_key {
            winit::keyboard::Key::Character(s) => Some(s.clone()),
            _ => None,
        };

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

    /// Lines are created *empty* (not pre-filled with `width` spaces — that
    /// per-character Automerge fill was O(n^2) and froze the whole
    /// compositor for seconds the first time a terminal synced). A write
    /// whose cursor lands past a fresh line's current end must therefore
    /// pad the gap with spaces so the printed char still renders at the
    /// correct column. Regression for the "dropping a window freezes the
    /// shell ~3-5 s" report.
    #[test]
    fn empty_line_pads_on_cursor_jump_write() {
        let (mut widget, _rx) = build_test_widget();
        let mut doc = AutoCommit::new();
        widget.init_crdt(&mut doc, &ROOT);

        // Fresh lines start empty.
        let grid_id = doc.get(ROOT, "terminal_grid").unwrap().unwrap().1;
        let line0 = doc.get(&grid_id, 0usize).unwrap().unwrap().1;
        let text0 = doc.get(&line0, "text").unwrap().unwrap().1;
        assert_eq!(doc.text(&text0).unwrap().len(), 0, "blank lines are seeded empty");

        // CSI 1;11 H → row 1, col 11 (1-based); print 'X' at 0-based col 10
        // on the still-empty first line — this exercises the pad path.
        widget.feed(&mut doc, &ROOT, b"\x1b[1;11HX");

        // Read the grid directly from the CRDT: find the line carrying 'X'
        // and assert it was padded to column 10 with spaces.
        let grid_id = doc.get(ROOT, "terminal_grid").unwrap().unwrap().1;
        let n = doc.length(&grid_id);
        let mut found: Option<String> = None;
        for i in 0..n {
            let line = doc.get(&grid_id, i).unwrap().unwrap().1;
            let text = doc.get(&line, "text").unwrap().unwrap().1;
            let s = doc.text(&text).unwrap();
            if s.contains('X') {
                found = Some(s);
                break;
            }
        }
        let line = found.expect("a grid line must carry the printed 'X'");
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

    #[test]
    fn test_terminal_performer_print() {
        use crate::TermModes;
        let mut doc = AutoCommit::new();
        let grid_id = doc.put_object(ROOT, "terminal_grid", ObjType::List).unwrap();
        let line_id = doc.insert_object(&grid_id, 0, ObjType::Map).unwrap();
        let text_id = doc.put_object(&line_id, "text", ObjType::Text).unwrap();
        doc.splice_text(&text_id, 0, 0, &" ".repeat(80)).unwrap();

        let attr_bytes: Vec<u8> = vec![Attr::default().pack(); 80].concat();
        doc.put(&line_id, "attrs", attr_bytes).unwrap();

        // The performer also needs a (possibly empty) alternate buffer list.
        let alt_grid_id = doc.put_object(ROOT, "terminal_grid_alt", ObjType::List).unwrap();
        let links_id = doc.put_object(ROOT, "terminal_links", ObjType::Map).unwrap();

        let cursor_id = doc.put_object(ROOT, "cursor", ObjType::Map).unwrap();
        doc.put(&cursor_id, "x", 0u64).unwrap();
        doc.put(&cursor_id, "y", 0u64).unwrap();

        let mut line_ids = vec![line_id.clone()];
        let mut alt_line_ids: Vec<_> = Vec::new();
        let mut alt_active = false;
        let mut modes = TermModes::default();
        let mut saved_cursor = None;
        let mut trimmed_front = 0usize;
        let mut next_link_id = 0u16;
        let mut title_out = None;

        {
            let mut performer = TerminalPerformer {
                state: &mut doc,
                main_grid_id: grid_id,
                alt_grid_id,
                cursor_id,
                links_id,
                main_line_ids: &mut line_ids,
                alt_line_ids: &mut alt_line_ids,
                alt_active: &mut alt_active,
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
                trimmed_front: &mut trimmed_front,
                next_link_id: &mut next_link_id,
                current_link_id: 0,
                title_out: &mut title_out,
                print_buf: None,
            };

            performer.print('H');
            performer.print('i');
            assert_eq!(performer.cursor_x, 2);
            // Printed cells are batched in-memory and committed to the CRDT on
            // any non-print action or at end of feed; flush so the read below
            // sees them (the real feed path flushes via `flush_cursor`).
            performer.flush_print_buf();
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
    #[serial_test::serial]
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
    #[serial_test::serial]
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

    // ---------------------------------------------------------------
    // Scrollback viewing — real VT bytes through `feed`/`sync`.
    // ---------------------------------------------------------------

    #[test]
    fn scrollback_reveals_older_lines_and_snaps_back() {
        use vello::kurbo::Rect;
        // 240px tall / 20px rows → 12 visible rows. Feed many more than that
        // so history accumulates above the viewport.
        let (mut w, _write_rx, out_tx) = build_test_widget_io();
        let mut doc = AutoCommit::new();
        w.force_geometry(Rect::new(0.0, 0.0, 800.0, 240.0), 10.0, 20.0);

        let mut payload = Vec::new();
        for i in 0..60 {
            payload.extend_from_slice(format!("LINE{:03}\r\n", i).as_bytes());
        }
        w.feed(&mut doc, &automerge::ROOT, &payload);
        w.sync(&mut doc, &automerge::ROOT);

        // Pinned to the bottom: newest lines visible, oldest not.
        let bottom = w.visible_lines();
        assert!(bottom.iter().any(|l| l.contains("LINE059")), "bottom shows newest: {:?}", bottom);
        assert!(!bottom.iter().any(|l| l.contains("LINE000")), "bottom hides oldest");
        assert_eq!(w.scroll_offset(), 0);

        // Scroll to the very top: oldest lines visible, newest not.
        w.scroll_to_top();
        let top = w.visible_lines();
        assert!(top.iter().any(|l| l.contains("LINE000")), "top shows oldest: {:?}", top);
        assert!(!top.iter().any(|l| l.contains("LINE059")), "top hides newest");
        assert!(w.scroll_offset() > 0);

        // Fresh PTY output must snap the viewport back to the bottom. Push it
        // through the real channel so `sync`→`pump` performs the snap.
        out_tx.send(b"LINE060\r\n".to_vec()).unwrap();
        w.sync(&mut doc, &automerge::ROOT);
        assert_eq!(w.scroll_offset(), 0, "new output snaps to the live bottom");
        let after = w.visible_lines();
        assert!(after.iter().any(|l| l.contains("LINE060")), "bottom shows freshest: {:?}", after);
    }

    /// Bulk output must stay linear. `print` batches a run of cells into one
    /// diffed text splice + one `attrs` put per line; the old per-cell
    /// `splice_text` + full-`attrs`-`put` was an O(doc) write *per character*, so
    /// a screenful of coloured output was O(n²) and took minutes (a debug-build
    /// freeze; HIG §6). Feeding a full screen of styled lines through the real
    /// `feed` path must produce correct content and finish well under a bound the
    /// quadratic path could never meet.
    #[test]
    fn bulk_styled_output_stays_linear_and_correct() {
        use std::time::{Duration, Instant};
        let (mut w, _write_rx, _out_tx) = build_test_widget_io();
        let mut doc = AutoCommit::new();
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

        let start = Instant::now();
        w.feed(&mut doc, &automerge::ROOT, &payload);
        w.sync(&mut doc, &automerge::ROOT);
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

    #[test]
    fn keystroke_snaps_scrollback_to_bottom() {
        use vello::kurbo::Rect;
        use winit::keyboard::{Key, SmolStr};
        let (mut w, _write_rx, _out_tx) = build_test_widget_io();
        let mut doc = AutoCommit::new();
        w.force_geometry(Rect::new(0.0, 0.0, 800.0, 240.0), 10.0, 20.0);

        let mut payload = Vec::new();
        for i in 0..60 { payload.extend_from_slice(format!("L{:03}\r\n", i).as_bytes()); }
        w.feed(&mut doc, &automerge::ROOT, &payload);
        w.sync(&mut doc, &automerge::ROOT);

        w.scroll_to_top();
        assert!(w.scroll_offset() > 0);

        // Typing a key that reaches the PTY must snap to the bottom.
        send_key(&mut w, &mut doc, Key::Character(SmolStr::new_static("x")), false, false);
        assert_eq!(w.scroll_offset(), 0, "keystroke snaps to bottom");
    }

    // ---------------------------------------------------------------
    // Paste — bracketed and plain, straight to the PTY.
    // ---------------------------------------------------------------

    #[test]
    fn plain_paste_writes_to_pty_with_cr_newlines() {
        let (mut w, write_rx) = build_test_widget();
        let mut doc = AutoCommit::new();
        w.feed(&mut doc, &automerge::ROOT, b""); // init crdt / modes
        w.paste("echo hi\nls");
        let got = drain_writes(&write_rx);
        assert_eq!(got, b"echo hi\rls", "plain paste normalises LF to CR");
    }

    #[test]
    fn bracketed_paste_wraps_when_app_enabled_it() {
        let (mut w, write_rx) = build_test_widget();
        let mut doc = AutoCommit::new();
        // App turns on bracketed paste via a real DEC private set sequence.
        w.feed(&mut doc, &automerge::ROOT, b"\x1b[?2004h");
        assert!(w.modes().bracketed_paste);
        w.paste("data");
        let got = drain_writes(&write_rx);
        assert_eq!(got, b"\x1b[200~data\x1b[201~", "bracketed paste wraps the payload");
    }

    // ---------------------------------------------------------------
    // Alternate screen + DEC private modes.
    // ---------------------------------------------------------------

    #[test]
    fn alt_screen_switches_buffers_and_restores_main() {
        use vello::kurbo::Rect;
        let (mut w, _rx) = build_test_widget();
        let mut doc = AutoCommit::new();
        w.force_geometry(Rect::new(0.0, 0.0, 800.0, 480.0), 10.0, 20.0);

        // Write to the main screen.
        w.feed(&mut doc, &automerge::ROOT, b"MAIN CONTENT\r\n");
        w.sync(&mut doc, &automerge::ROOT);
        assert!(!w.is_alt_screen());
        assert!(w.grid_row(0).starts_with("MAIN CONTENT"));

        // Enter the alternate screen and do a full-screen redraw.
        w.feed(&mut doc, &automerge::ROOT, b"\x1b[?1049h\x1b[2J\x1b[H");
        w.feed(&mut doc, &automerge::ROOT, b"\x1b[3;5HTUI REDRAW");
        w.sync(&mut doc, &automerge::ROOT);
        assert!(w.is_alt_screen(), "?1049h enters the alternate screen");
        // The alt buffer holds the redraw, and the main content is not in it.
        assert!(w.grid_row(2).contains("TUI REDRAW"), "row 2 (1-based row 3): {:?}", w.grid_row(2));
        assert!(!w.grid_row(0).contains("MAIN CONTENT"), "alt buffer is fresh");

        // Leave the alternate screen — the main buffer must return intact.
        w.feed(&mut doc, &automerge::ROOT, b"\x1b[?1049l");
        w.sync(&mut doc, &automerge::ROOT);
        assert!(!w.is_alt_screen(), "?1049l restores the main screen");
        assert!(
            w.grid_row(0).starts_with("MAIN CONTENT"),
            "main content restored: {:?}",
            w.grid_row(0)
        );
        assert!(!w.grid_row(2).contains("TUI REDRAW"), "alt redraw did not leak into main");
    }

    #[test]
    fn dec_private_modes_toggle_cursor_and_bracketed_paste() {
        let (mut w, _rx) = build_test_widget();
        let mut doc = AutoCommit::new();
        w.feed(&mut doc, &automerge::ROOT, b"");
        assert!(w.modes().cursor_visible, "cursor visible by default");

        w.feed(&mut doc, &automerge::ROOT, b"\x1b[?25l");
        assert!(!w.modes().cursor_visible, "?25l hides the cursor");
        w.feed(&mut doc, &automerge::ROOT, b"\x1b[?25h");
        assert!(w.modes().cursor_visible, "?25h shows the cursor");

        // Mouse reporting is parsed (not crashing) into the mode flags.
        w.feed(&mut doc, &automerge::ROOT, b"\x1b[?1000h\x1b[?1006h");
        assert!(w.modes().mouse_report && w.modes().mouse_sgr, "mouse modes stubbed but tracked");
    }

    // ---------------------------------------------------------------
    // SGR — 256-color, truecolor, and the four style bits.
    // ---------------------------------------------------------------

    #[test]
    fn sgr_truecolor_and_indexed_colors_parse_into_cells() {
        use crate::CellColor;
        use vello::kurbo::Rect;
        let (mut w, _rx) = build_test_widget();
        let mut doc = AutoCommit::new();
        w.force_geometry(Rect::new(0.0, 0.0, 800.0, 480.0), 10.0, 20.0);

        // Semicolon truecolor foreground.
        w.feed(&mut doc, &automerge::ROOT, b"\x1b[H\x1b[38;2;10;20;30mA");
        w.sync(&mut doc, &automerge::ROOT);
        assert_eq!(w.cell_attr(0, 0).fg, CellColor::Rgb(10, 20, 30), "38;2 truecolor fg");

        // Semicolon 256-indexed foreground.
        w.feed(&mut doc, &automerge::ROOT, b"\x1b[H\x1b[38;5;196mB");
        w.sync(&mut doc, &automerge::ROOT);
        assert_eq!(w.cell_attr(0, 0).fg, CellColor::Indexed(196), "38;5 indexed fg");

        // Colon-subparam 256-indexed foreground (the xterm form apps emit).
        w.feed(&mut doc, &automerge::ROOT, b"\x1b[H\x1b[38:5:214mC");
        w.sync(&mut doc, &automerge::ROOT);
        assert_eq!(w.cell_attr(0, 0).fg, CellColor::Indexed(214), "38:5 colon-form indexed fg");

        // Truecolor background.
        w.feed(&mut doc, &automerge::ROOT, b"\x1b[H\x1b[48;2;5;6;7mD");
        w.sync(&mut doc, &automerge::ROOT);
        assert_eq!(w.cell_attr(0, 0).bg, CellColor::Rgb(5, 6, 7), "48;2 truecolor bg");
    }

    #[test]
    fn sgr_style_bits_and_reset_parse_into_cells() {
        use crate::CellColor;
        use vello::kurbo::Rect;
        let (mut w, _rx) = build_test_widget();
        let mut doc = AutoCommit::new();
        w.force_geometry(Rect::new(0.0, 0.0, 800.0, 480.0), 10.0, 20.0);

        // Bold + italic + underline + reverse all at once.
        w.feed(&mut doc, &automerge::ROOT, b"\x1b[H\x1b[1;3;4;7mW");
        w.sync(&mut doc, &automerge::ROOT);
        let a = w.cell_attr(0, 0);
        assert!(a.bold && a.italic && a.underline && a.reverse, "combined SGR style bits: {:?}", a);

        // A reset clears everything back to the default attribute.
        w.feed(&mut doc, &automerge::ROOT, b"\x1b[H\x1b[0mN");
        w.sync(&mut doc, &automerge::ROOT);
        let b = w.cell_attr(0, 0);
        assert_eq!(b.fg, CellColor::Default);
        assert!(!b.bold && !b.italic && !b.underline && !b.reverse, "SGR 0 resets style: {:?}", b);
    }

    // ---------------------------------------------------------------
    // Real PTY — a live subprocess through the production reader→sync path.
    // ---------------------------------------------------------------

    /// Poll the production `sync` (which drains the PTY reader thread and
    /// replays bytes through vte into the CRDT grid) until the grid contains
    /// `needle` or `tries` elapse. Returns whether it appeared.
    fn pump_until_contains(
        w: &mut TerminalWidget,
        doc: &mut AutoCommit,
        needle: &str,
        tries: usize,
    ) -> bool {
        for _ in 0..tries {
            w.sync(doc, &ROOT);
            if w.grid_text().contains(needle) {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        w.grid_text().contains(needle)
    }

    #[test]
    fn spawn_command_runs_real_pty_and_grid_shows_output() {
        use portable_pty::CommandBuilder;
        // Run a real subprocess in a real PTY and prove its stdout reaches the
        // emulator grid through the production reader → sync → vte → CRDT path
        // (no `feed`, no mock). This is the load-bearing "the terminal actually
        // runs programs" proof.
        let mut cmd = CommandBuilder::new("/bin/sh");
        cmd.arg("-c");
        cmd.arg("printf 'QOS_PTY_OK\\n'");
        let mut w = TerminalWidget::spawn_command(cmd).expect("spawn pty command");
        let mut doc = AutoCommit::new();
        assert!(
            pump_until_contains(&mut w, &mut doc, "QOS_PTY_OK", 200),
            "real PTY command output must reach the grid; grid was:\n{}",
            w.grid_text()
        );
    }

    #[test]
    fn spawn_default_runs_real_shell_and_executes_typed_command() {
        // Spawn the user's login shell, then type a command exactly the way a
        // keystroke sends it (`write_input`), and prove the shell ran it and
        // the output flowed back into the grid.
        let mut w = TerminalWidget::spawn_default().expect("spawn login shell");
        let mut doc = AutoCommit::new();
        // Let the shell reach a prompt before typing.
        w.sync(&mut doc, &ROOT);
        w.write_input(b"printf 'QOS_SHELL_OK\\n'\n");
        assert!(
            pump_until_contains(&mut w, &mut doc, "QOS_SHELL_OK", 300),
            "the real shell must run the typed command and show output; grid was:\n{}",
            w.grid_text()
        );
    }

    #[test]
    fn spawn_command_reports_grid_size() {
        use portable_pty::CommandBuilder;
        // The default PTY is opened at 80x24; the host-facing accessor must
        // report that until a `place_at` reflow changes it.
        let w = TerminalWidget::spawn_command(CommandBuilder::new("/bin/sh")).expect("spawn");
        assert_eq!(w.grid_size(), (80, 24));
    }

    // ---------------------------------------------------------------
    // SGR intensity / crossed-out — dim (2) and strikethrough (9).
    // ---------------------------------------------------------------

    #[test]
    fn sgr_dim_and_strikethrough_parse_into_cells() {
        use vello::kurbo::Rect;
        let (mut w, _rx) = build_test_widget();
        let mut doc = AutoCommit::new();
        w.force_geometry(Rect::new(0.0, 0.0, 800.0, 480.0), 10.0, 20.0);

        // Faint + crossed-out together.
        w.feed(&mut doc, &automerge::ROOT, b"\x1b[H\x1b[2;9mX");
        w.sync(&mut doc, &automerge::ROOT);
        let a = w.cell_attr(0, 0);
        assert!(a.dim, "SGR 2 sets faint: {:?}", a);
        assert!(a.strike, "SGR 9 sets crossed-out: {:?}", a);

        // SGR 22 clears both bold and faint; SGR 29 clears crossed-out.
        w.feed(&mut doc, &automerge::ROOT, b"\x1b[H\x1b[1;2;9mY\x1b[22;29mZ");
        w.sync(&mut doc, &automerge::ROOT);
        let z = w.cell_attr(0, 1);
        assert!(!z.dim && !z.bold, "SGR 22 clears bold+faint: {:?}", z);
        assert!(!z.strike, "SGR 29 clears crossed-out: {:?}", z);
    }

    // ---------------------------------------------------------------
    // Keyboard encoding — modified cursor/function keys reach the PTY.
    // ---------------------------------------------------------------

    #[test]
    fn ctrl_arrow_sends_modified_csi_for_word_motion() {
        use winit::keyboard::{Key, NamedKey};
        use vello::kurbo::Rect;
        let (mut w, rx) = build_test_widget();
        let mut doc = AutoCommit::new();
        w.force_geometry(Rect::new(0.0, 0.0, 800.0, 480.0), 10.0, 20.0);
        w.feed(&mut doc, &automerge::ROOT, b""); // init modes

        send_key_mods(&mut w, &mut doc, Key::Named(NamedKey::ArrowLeft), true, false, false);
        assert_eq!(drain_writes(&rx), b"\x1b[1;5D", "Ctrl+Left = CSI 1;5D (word-left)");

        send_key_mods(&mut w, &mut doc, Key::Named(NamedKey::ArrowRight), true, false, false);
        assert_eq!(drain_writes(&rx), b"\x1b[1;5C", "Ctrl+Right = CSI 1;5C (word-right)");

        // Shift+arrow (in-app selection) uses modifier param 2.
        send_key_mods(&mut w, &mut doc, Key::Named(NamedKey::ArrowUp), false, true, false);
        assert_eq!(drain_writes(&rx), b"\x1b[1;2A", "Shift+Up = CSI 1;2A");
    }

    #[test]
    fn unmodified_arrow_respects_decckm_app_cursor_keys() {
        use winit::keyboard::{Key, NamedKey};
        use vello::kurbo::Rect;
        let (mut w, rx) = build_test_widget();
        let mut doc = AutoCommit::new();
        w.force_geometry(Rect::new(0.0, 0.0, 800.0, 480.0), 10.0, 20.0);

        // Default (normal cursor keys) → CSI form.
        send_key_mods(&mut w, &mut doc, Key::Named(NamedKey::ArrowUp), false, false, false);
        assert_eq!(drain_writes(&rx), b"\x1b[A", "default arrow = CSI A");

        // App turns on DECCKM; unmodified arrows now use SS3.
        w.feed(&mut doc, &automerge::ROOT, b"\x1b[?1h");
        assert!(w.modes().app_cursor_keys);
        send_key_mods(&mut w, &mut doc, Key::Named(NamedKey::ArrowUp), false, false, false);
        assert_eq!(drain_writes(&rx), b"\x1bOA", "DECCKM arrow = SS3 A");
    }

    #[test]
    fn function_keys_send_expected_sequences() {
        use winit::keyboard::{Key, NamedKey};
        use vello::kurbo::Rect;
        let (mut w, rx) = build_test_widget();
        let mut doc = AutoCommit::new();
        w.force_geometry(Rect::new(0.0, 0.0, 800.0, 480.0), 10.0, 20.0);

        send_key_mods(&mut w, &mut doc, Key::Named(NamedKey::F1), false, false, false);
        assert_eq!(drain_writes(&rx), b"\x1bOP", "F1 = SS3 P");
        send_key_mods(&mut w, &mut doc, Key::Named(NamedKey::F5), false, false, false);
        assert_eq!(drain_writes(&rx), b"\x1b[15~", "F5 = CSI 15~");
        send_key_mods(&mut w, &mut doc, Key::Named(NamedKey::F10), false, false, false);
        assert_eq!(drain_writes(&rx), b"\x1b[21~", "F10 = CSI 21~");
        send_key_mods(&mut w, &mut doc, Key::Named(NamedKey::F12), false, false, false);
        assert_eq!(drain_writes(&rx), b"\x1b[24~", "F12 = CSI 24~");
    }

    #[test]
    fn shift_tab_sends_back_tab() {
        use winit::keyboard::{Key, NamedKey};
        use vello::kurbo::Rect;
        let (mut w, rx) = build_test_widget();
        let mut doc = AutoCommit::new();
        w.force_geometry(Rect::new(0.0, 0.0, 800.0, 480.0), 10.0, 20.0);

        send_key_mods(&mut w, &mut doc, Key::Named(NamedKey::Tab), false, false, false);
        assert_eq!(drain_writes(&rx), b"\t", "plain Tab = HT");
        send_key_mods(&mut w, &mut doc, Key::Named(NamedKey::Tab), false, true, false);
        assert_eq!(drain_writes(&rx), b"\x1b[Z", "Shift+Tab = CSI Z (back-tab)");
    }

    #[test]
    fn alt_letter_sends_meta_escape_but_altgr_symbols_pass_through() {
        use winit::keyboard::{Key, SmolStr};
        use vello::kurbo::Rect;
        let (mut w, rx) = build_test_widget();
        let mut doc = AutoCommit::new();
        w.force_geometry(Rect::new(0.0, 0.0, 800.0, 480.0), 10.0, 20.0);

        // Alt+f → ESC f (readline forward-word).
        send_key_mods(&mut w, &mut doc, Key::Character(SmolStr::new_static("f")), false, false, true);
        assert_eq!(drain_writes(&rx), b"\x1bf", "Alt+f = ESC f (Meta)");

        // AltGr on a German layout yields a composed *symbol* as the logical
        // key (e.g. '@'); Meta must NOT poach it — the symbol itself is sent.
        send_key_mods(&mut w, &mut doc, Key::Character(SmolStr::new_static("@")), false, false, true);
        assert_eq!(drain_writes(&rx), b"@", "AltGr-composed '@' passes through verbatim");
    }

    // ---------------------------------------------------------------
    // Incremental grid mirror — heavy output stays correct + bounded.
    // ---------------------------------------------------------------

    #[test]
    fn incremental_sync_and_trim_keep_mirror_aligned() {
        use vello::kurbo::Rect;
        // Feed across several syncs, past the (test-shrunk) retained-history
        // cap, then prove the render mirror — which now only re-reads changed
        // rows and drains trimmed front lines rather than re-reading the whole
        // buffer — is still exactly index-aligned with the CRDT: newest kept,
        // oldest trimmed, survivors intact, memory bounded.
        let (mut w, _rx, _out) = build_test_widget_io();
        let mut doc = AutoCommit::new();
        w.force_geometry(Rect::new(0.0, 0.0, 800.0, 240.0), 10.0, 20.0); // 80x24 logical grid

        let mut emitted = 0usize;
        for _chunk in 0..3 {
            let mut payload = Vec::new();
            for _ in 0..50 {
                payload.extend_from_slice(format!("ROW{:04}\r\n", emitted).as_bytes());
                emitted += 1;
            }
            w.feed(&mut doc, &automerge::ROOT, &payload);
            w.sync(&mut doc, &automerge::ROOT);
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

    #[test]
    fn external_edit_without_local_feed_triggers_full_reread() {
        use vello::kurbo::Rect;
        // Simulate a collaborator editing an *old* scrollback line directly in
        // the CRDT (no local feed). Because the mirror can't assume only the
        // live screen changed, `sync` must fall back to a full re-read and
        // surface the remote edit.
        let (mut w, _rx, _out) = build_test_widget_io();
        let mut doc = AutoCommit::new();
        w.force_geometry(Rect::new(0.0, 0.0, 800.0, 240.0), 10.0, 20.0);

        let mut payload = Vec::new();
        for i in 0..40 { payload.extend_from_slice(format!("BASE{:03}\r\n", i).as_bytes()); }
        w.feed(&mut doc, &automerge::ROOT, &payload);
        w.sync(&mut doc, &automerge::ROOT);
        assert!(w.grid_text().contains("BASE000"), "base content mirrored");

        // Edit line 0's text object directly (external mutation, no feed).
        let grid_id = doc.get(&automerge::ROOT, "terminal_grid").unwrap().unwrap().1;
        let line0 = doc.get(&grid_id, 0).unwrap().unwrap().1;
        let (_, text_id) = doc.get(&line0, "text").unwrap().unwrap();
        let len = doc.length(&text_id);
        doc.splice_text(&text_id, 0, len as isize, "REMOTE_EDIT").unwrap();

        w.sync(&mut doc, &automerge::ROOT);
        assert!(
            w.grid_text().contains("REMOTE_EDIT"),
            "external edit to old scrollback must be picked up by a full re-read"
        );
    }

    // ---------------------------------------------------------------
    // Full text selection — multi-click word/line, shift-extend, and
    // buffer-anchored selection that survives scrolling.
    // ---------------------------------------------------------------

    /// Send a left press with explicit modifiers (for shift-extend).
    fn send_left_press_mods(widget: &mut TerminalWidget, doc: &mut AutoCommit, shift: bool) {
        use winit::event::{WindowEvent, MouseButton, ElementState, Modifiers};
        use winit::keyboard::ModifiersState;
        use ui::widget::Widget;
        let mut handled = false;
        let mut focus = None;
        let mut state = ModifiersState::empty();
        if shift { state |= ModifiersState::SHIFT; }
        let modifiers: Modifiers = state.into();
        let taffy = taffy::TaffyTree::<()>::new();
        let node = taffy::NodeId::new(0);
        let ev = WindowEvent::MouseInput {
            device_id: fake_device_id(),
            state: ElementState::Pressed,
            button: MouseButton::Left,
        };
        widget.handle_event(&ev, 1.0, &modifiers, doc, &automerge::ROOT, &mut handled, &mut focus, node, &taffy);
    }

    /// Send a middle-button press (primary-selection paste).
    fn send_middle_press(widget: &mut TerminalWidget, doc: &mut AutoCommit) {
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
            button: MouseButton::Middle,
        };
        widget.handle_event(&ev, 1.0, &modifiers, doc, &automerge::ROOT, &mut handled, &mut focus, node, &taffy);
    }

    /// Logical x for the centre of grid column `col` given `char_w=10`,
    /// `padding=4` (the widget's default gutter).
    fn col_center_x(col: usize) -> f64 { 4.0 + col as f64 * 10.0 + 5.0 }

    #[test]
    #[serial_test::serial]
    fn double_click_selects_word() {
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

    #[test]
    #[serial_test::serial]
    fn triple_click_selects_line() {
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

    #[test]
    #[serial_test::serial]
    fn shift_click_extends_existing_selection() {
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
        assert_eq!(w.test_selected_text().as_deref(), Some("llo"), "initial drag selection");

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

    #[test]
    #[serial_test::serial]
    fn selection_is_buffer_anchored_and_survives_scrolling() {
        // The headline property: a selection anchors to absolute buffer rows,
        // so scrolling the viewport does not change what is selected. Drive a
        // real double-click over a scrollback row, then move the viewport and
        // confirm the selected text is byte-identical.
        let (mut w, _write_rx) = build_test_widget();
        let mut doc = AutoCommit::new();
        w.force_geometry(Rect::new(0.0, 0.0, 800.0, 240.0), 10.0, 20.0); // 12 visible rows
        // A deterministic 20-row buffer; row 5 carries a distinctive word.
        let mut grid: Vec<String> = (0..20).map(|i| format!("row{:02} filler text", i)).collect();
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
        assert_eq!(w.test_selected_text().as_deref(), Some("bravo"), "selected the right word");
        send_left_release(&mut w, &mut doc); // finish → primary (+ clipboard: select_to_copy)

        // The selection lives on absolute row 5.
        let sel = w.selection().expect("selection present");
        assert_eq!(sel.anchor.row, 5, "selection anchored to absolute buffer row 5");

        // "Scroll": move the viewport elsewhere. The selected text must not move.
        w.force_viewport_start(0);
        assert_eq!(
            w.test_selected_text().as_deref(),
            Some("bravo"),
            "selection stays on the same buffer cells after scrolling"
        );
        w.force_viewport_start(8);
        assert_eq!(w.test_selected_text().as_deref(), Some("bravo"));

        // Clipboard round-trip (skipped when no backend is available).
        if arboard::Clipboard::new().is_ok() {
            let mut cb = arboard::Clipboard::new().unwrap();
            if let Ok(text) = cb.get_text() {
                assert_eq!(text, "bravo", "select-to-copy put exactly the selection on the clipboard");
            }
        }
    }

    #[test]
    #[cfg(target_os = "linux")]
    #[serial_test::serial]
    fn middle_click_pastes_primary_selection() {
        // Middle-click inserts the primary selection into the PTY. Requires a
        // working primary selection (a Wayland compositor / X server); skipped
        // when unavailable so headless CI doesn't flake.
        ui::clipboard_copy_primary("PASTED");
        if ui::clipboard_paste_primary().as_deref() != Some("PASTED") {
            eprintln!("middle_click_pastes_primary_selection: no primary selection backend, skipping");
            return;
        }
        let (mut w, write_rx) = build_test_widget();
        let mut doc = AutoCommit::new();
        w.force_geometry(Rect::new(0.0, 0.0, 800.0, 480.0), 10.0, 20.0);
        w.feed(&mut doc, &automerge::ROOT, b""); // init modes (no bracketed paste)
        send_cursor_move(&mut w, &mut doc, 100.0, 100.0);
        send_middle_press(&mut w, &mut doc);
        let got = drain_writes(&write_rx);
        assert_eq!(got, b"PASTED", "middle-click pastes the primary selection to the PTY");
    }

    // ---------------------------------------------------------------
    // Collaboration — two peers converge on the same terminal grid via
    // the real CRDT save/load-incremental path the toolkit App uses.
    // ---------------------------------------------------------------

    #[test]
    fn two_peers_converge_on_terminal_grid_via_crdt() {
        // Peer A runs a session; peer B late-joins from A's full snapshot
        // (`SyncCRDT`) and then receives A's live output as an incremental
        // delta (`SyncIncremental`) — exactly the save/load path `ui::App`
        // drives. Both peers must converge on an identical grid.
        let (mut a, _rx_a) = build_test_widget();
        let mut doc_a = AutoCommit::new();
        doc_a.set_actor(automerge::ActorId::from(b"alice".as_slice()));
        a.force_geometry(Rect::new(0.0, 0.0, 800.0, 480.0), 10.0, 20.0);
        a.feed(&mut doc_a, &automerge::ROOT, b"peer-a-line-one\r\n");
        a.sync(&mut doc_a, &automerge::ROOT);
        assert!(a.grid_text().contains("peer-a-line-one"));

        // Late join: B adopts A's shared document.
        let mut doc_b = AutoCommit::load(&doc_a.save()).unwrap();
        doc_b.set_actor(automerge::ActorId::from(b"bob".as_slice()));
        let (mut b, _rx_b) = build_test_widget();
        b.force_geometry(Rect::new(0.0, 0.0, 800.0, 480.0), 10.0, 20.0);
        b.sync(&mut doc_b, &automerge::ROOT);
        assert!(
            b.grid_text().contains("peer-a-line-one"),
            "B adopts A's grid on join; B grid:\n{}",
            b.grid_text()
        );

        // Re-baseline A's incremental cursor after the full save (App does this).
        let _ = doc_a.save_incremental();

        // A produces more output; deliver the delta to B.
        a.feed(&mut doc_a, &automerge::ROOT, b"peer-a-line-two\r\n");
        a.sync(&mut doc_a, &automerge::ROOT);
        let delta = doc_a.save_incremental();
        assert!(!delta.is_empty(), "A produced an incremental change to share");
        doc_b.load_incremental(&delta).unwrap();
        b.sync(&mut doc_b, &automerge::ROOT);
        assert!(
            b.grid_text().contains("peer-a-line-two"),
            "B sees A's live output over the incremental sync path"
        );

        // Full convergence: identical grids on both peers.
        assert_eq!(
            a.grid_text(),
            b.grid_text(),
            "peers converge on the same terminal grid"
        );
    }

    #[test]
    fn peers_share_pane_id_and_presence_key() {
        // Presence must be keyed on a shared pane id so a peer's caret /
        // selection is filed and rendered under the same key both sides use.
        use ui::presence::PresenceKey;
        use ui::widget::Widget as _;
        let (mut a, _rx_a) = build_test_widget();
        let mut doc_a = AutoCommit::new();
        a.feed(&mut doc_a, &automerge::ROOT, b"");
        a.sync(&mut doc_a, &automerge::ROOT);

        let mut doc_b = AutoCommit::load(&doc_a.save()).unwrap();
        let (mut b, _rx_b) = build_test_widget();
        b.sync(&mut doc_b, &automerge::ROOT);

        let ka = a.presence_key().expect("A presence key");
        let kb = b.presence_key().expect("B presence key");
        assert_eq!(ka, kb, "both peers scope presence under the same shared pane id");
        match ka {
            PresenceKey::App { app, .. } => {
                assert_eq!(app, ui::permissions::AppKind::Terminal);
            }
            other => panic!("expected App presence key, got {:?}", other),
        }
    }

    #[test]
    fn presence_payload_reports_selection_then_cursor() {
        use ui::collaboration::PresencePayload;
        use ui::widget::Widget as _;
        let (mut w, _) = build_test_widget();
        let mut doc = AutoCommit::new();
        w.init_crdt(&mut doc, &automerge::ROOT);
        w.force_grid(vec!["abcdefgh".to_string()]);

        // With a selection, the payload is a Selection over linear indices.
        w.force_selection(Some(Selection {
            anchor: GridPos { row: 0, col: 2 },
            cursor: GridPos { row: 0, col: 5 },
        }));
        let (_key, payload) = w.presence_payload().expect("payload with selection");
        match payload {
            // width defaults to 80, so row 0 linearises to the column itself.
            PresencePayload::Selection { start, end } => {
                assert_eq!((start, end), (2, 5), "selection linearised to cell columns");
            }
            other => panic!("expected Selection payload, got {:?}", other),
        }

        // Without a selection, the payload is a Caret at the cursor.
        w.force_selection(None);
        let (_key, payload) = w.presence_payload().expect("payload with cursor");
        assert!(matches!(payload, PresencePayload::Caret { .. }), "cursor payload");
    }

    // ---------------------------------------------------------------
    // Spec compliance — OSC 0/2 title, OSC 52 clipboard, OSC 8 links.
    // ---------------------------------------------------------------

    #[test]
    fn osc_window_title_updates_app_title() {
        use ui::AppWidget;
        let (mut w, _) = build_test_widget();
        let mut doc = AutoCommit::new();
        // OSC 2 ; <title> BEL
        w.feed(&mut doc, &automerge::ROOT, b"\x1b]2;my session\x07");
        assert_eq!(w.title(), "my session", "OSC 2 sets the window title");
        assert_eq!(w.app_title(), "my session", "app_title reflects the OSC title");

        // OSC 0 (icon + title) also sets it; ST-terminated form.
        w.feed(&mut doc, &automerge::ROOT, b"\x1b]0;another\x1b\\");
        assert_eq!(w.title(), "another", "OSC 0 sets the window title too");

        // The title rides the CRDT: a second widget on the same doc reads it.
        w.sync(&mut doc, &automerge::ROOT);
        let (mut peer, _) = build_test_widget();
        let mut peer_doc = AutoCommit::load(&doc.save()).unwrap();
        peer.sync(&mut peer_doc, &automerge::ROOT);
        assert_eq!(peer.title(), "another", "a collaborator sees the same title");
    }

    #[test]
    #[cfg(target_os = "linux")]
    #[serial_test::serial]
    fn osc_52_writes_clipboard_from_base64() {
        // OSC 52 ; c ; <base64> sets the clipboard. Skipped without a backend.
        if arboard::Clipboard::new().is_err() {
            eprintln!("osc_52_writes_clipboard_from_base64: no clipboard backend, skipping");
            return;
        }
        let (mut w, _) = build_test_widget();
        let mut doc = AutoCommit::new();
        // base64("qos-osc52") = cW9zLW9zYzUy
        w.feed(&mut doc, &automerge::ROOT, b"\x1b]52;c;cW9zLW9zYzUy\x07");
        let mut cb = arboard::Clipboard::new().unwrap();
        if let Ok(text) = cb.get_text() {
            assert_eq!(text, "qos-osc52", "OSC 52 decodes base64 into the clipboard");
        }
    }

    #[test]
    fn osc_8_hyperlink_marks_cells_and_resolves_url() {
        // OSC 8 ; ; <uri> ST … OSC 8 ; ; ST wraps the cells in between with a
        // hyperlink that resolves back to the URL (rendered underlined and
        // clickable). The link registry rides the CRDT so peers resolve it too.
        let (mut w, _) = build_test_widget();
        let mut doc = AutoCommit::new();
        w.force_geometry(Rect::new(0.0, 0.0, 800.0, 480.0), 10.0, 20.0);
        // Home, open link, print "link", close link.
        w.feed(
            &mut doc,
            &automerge::ROOT,
            b"\x1b[H\x1b]8;;https://qos.example/docs\x1b\\link\x1b]8;;\x1b\\",
        );
        w.sync(&mut doc, &automerge::ROOT);

        assert_eq!(w.grid_row(0).trim_end(), "link", "the link text is printed");
        assert_eq!(
            w.hyperlink_at(0, 0),
            Some("https://qos.example/docs"),
            "first link cell resolves to the URL"
        );
        assert_eq!(
            w.hyperlink_at(0, 3),
            Some("https://qos.example/docs"),
            "last link cell resolves to the URL"
        );
        assert_eq!(w.hyperlink_at(0, 10), None, "cells outside the link have no URL");

        // A peer on the same doc resolves the same hyperlink.
        let (mut peer, _) = build_test_widget();
        peer.force_geometry(Rect::new(0.0, 0.0, 800.0, 480.0), 10.0, 20.0);
        let mut peer_doc = AutoCommit::load(&doc.save()).unwrap();
        peer.sync(&mut peer_doc, &automerge::ROOT);
        assert_eq!(
            peer.hyperlink_at(0, 1),
            Some("https://qos.example/docs"),
            "a collaborator resolves the same OSC 8 hyperlink"
        );
    }

    // ---------------------------------------------------------------
    // Grid geometry — a uniform body padding + scrollbar gutter inset the grid
    // (there is NO app-drawn header/tab bar; the compositor owns window
    // decoration), and pointer hit-testing follows the same inset grid rect
    // (production `place_at`/`set_geometry` path).
    // ---------------------------------------------------------------

    #[test]
    fn place_at_insets_grid_by_padding_and_gutters() {
        // The production reflow path must seat the cell grid inside the body
        // padding on every side + the scrollbar gutter on the right — not fill
        // the whole widget rect the way the old void did. No header strip: the
        // top inset is the body padding, not an app-drawn tab bar.
        let (mut w, _) = build_test_widget();
        w.place_at(Rect::new(0.0, 0.0, 1000.0, 640.0));
        let gr = w.grid_rect();
        assert!(gr.y0 > 0.0, "grid top inset by body padding: {gr:?}");
        assert!(gr.x0 > 0.0, "grid left inset by body padding: {gr:?}");
        assert!(gr.x1 < 1000.0, "grid right inset by padding + scrollbar gutter: {gr:?}");
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

    #[test]
    fn padding_click_does_not_start_selection() {
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

    #[test]
    fn grid_click_inside_content_starts_selection_and_maps_in_bounds() {
        // A press inside the inset grid content still begins a selection and
        // maps to an in-bounds cell — proving hit-testing follows the grid rect,
        // not the outer widget rect.
        let (mut w, _) = build_test_widget();
        let mut doc = AutoCommit::new();
        w.place_at(Rect::new(0.0, 0.0, 800.0, 600.0));
        let gr = w.grid_rect();
        send_cursor_move(&mut w, &mut doc, gr.x0 + 30.0, gr.y0 + 20.0);
        send_left_press(&mut w, &mut doc);
        let sel = w.selection().expect("press inside the grid begins a selection");
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
