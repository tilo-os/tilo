// The terminal's scrollback as a scroll view: the wheel and fingers move it
// pixel by pixel through the shared momentum and rubber band, and its frames
// say which rows a scroll only moved, so a host copies them. Included into
// `inner` beside `main_body.rs`.

/// The terminal's scrollback as a scroll view.
#[derive(Default)]
struct ScrollbackView {
    /// How far past the top of its first row the view is scrolled, in
    /// logical pixels, below one row: fingers and a glide move the
    /// scrollback pixel by pixel. Zero at the live output.
    sub: f64,
    /// Wheel and touchpad motion over the scrollback: a notch moves at once
    /// and glides on, fingers move the rows 1:1 and a flick glides on after
    /// they lift, through the rubber band past either end.
    glide: ui::ScrollMomentum,
    /// The theme's motion, where the terminal last drew.
    motion: ui::tokens::EffectiveMotion,
    /// What the frame being drawn shows of the scrollback, and what the
    /// frame on screen showed: a scroll copies the rows it only moved.
    shown: ui::scroll_copy::PaintedFrames,
    /// Whether the frame being drawn, and the frame on screen, painted the
    /// terminal's top layer (its context menu, the environments panel).
    overlay_drawn: bool,
    overlay_presented: bool,
}

impl TerminalWidget {
    /// How far the view stands below the top of the scrollback, in logical
    /// pixels: whole rows, and the part of a row it scrolled past
    /// (`view.sub`).
    fn scroll_from_top(&self) -> f64 {
        let max = self.max_scroll();
        max.saturating_sub(self.scroll_offset.min(max)) as f64 * self.char_h.max(1.0)
            + self.view.sub
    }

    /// Move the view `px` logical pixels toward the live output (back into
    /// history when negative), as far as the scrollback goes; returns how
    /// far it moved. Fingers and a glide move it pixel by pixel; the rows it
    /// shows are the scrollback's, whole.
    fn scroll_by_px(&mut self, px: f64) -> f64 {
        if self.alt_active || !px.is_finite() {
            return 0.0;
        }
        let row = self.char_h.max(1.0);
        let max = self.max_scroll();
        let before = self.scroll_from_top();
        let to = (before + px).clamp(0.0, max as f64 * row);
        let rows = (to / row).floor();
        let top = (rows as usize).min(max);
        self.scroll_offset = max - top;
        self.view.sub = if self.scroll_offset == 0 {
            0.0
        } else {
            to - rows * row
        };
        self.wheel_remainder_px = 0.0;
        to - before
    }

    /// Show the view `rows` rows back from the live output, at a row's top,
    /// stopping any glide.
    fn set_scroll_offset(&mut self, rows: usize) {
        self.scroll_offset = rows;
        self.view.sub = 0.0;
        self.wheel_remainder_px = 0.0;
        self.view.glide.stop();
    }

    /// Scroll the viewport back (toward older output) by `n` rows.
    pub fn scroll_up(&mut self, n: usize) {
        if self.alt_active {
            return;
        }
        self.set_scroll_offset((self.scroll_offset + n).min(self.max_scroll()));
    }
    /// Scroll the viewport forward (toward the live bottom) by `n` rows.
    pub fn scroll_down(&mut self, n: usize) {
        self.set_scroll_offset(self.scroll_offset.saturating_sub(n));
    }
    /// Jump to the oldest retained line.
    pub fn scroll_to_top(&mut self) {
        if self.alt_active {
            return;
        }
        self.set_scroll_offset(self.max_scroll());
    }
    /// Pin the viewport back to the live output.
    pub fn scroll_to_bottom(&mut self) {
        self.set_scroll_offset(0);
    }

    /// Step a glide after a flick or a wheel notch to the frame's time,
    /// from `update`. A glide that meets an end of the scrollback stops
    /// there.
    fn advance_glide(&mut self) {
        if !self.view.glide.is_active() {
            return;
        }
        let step = self.view.glide.advance(ui::motion::frame_now());
        if step != 0.0 {
            if self.scroll_by_px(step) == 0.0 {
                self.view.glide.stop();
            } else {
                ui::widget::note_view_only_pan();
            }
        }
    }

    /// Helper for [`Widget::handle_event`]'s `WindowEvent::MouseWheel` arm.
    /// Over the scrollback the wheel and fingers move the rows pixel by
    /// pixel and glide on; a program that reads the mouse, or the alternate
    /// screen, gets whole rows.
    fn handle_wheel_event(
        &mut self,
        delta: &MouseScrollDelta,
        phase: winit::event::TouchPhase,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        handled: &mut bool,
    ) {
        if !self.rect.contains(self.last_cursor_pos) {
            return;
        }
        let reports = self.modes.mouse_report
            && !modifiers.state().shift_key()
            && self.grid_rect.contains(self.last_cursor_pos);
        if !reports && !self.alt_active {
            self.wheel_scrollback(delta, phase, scale_factor);
            *handled = true;
            return;
        }
        self.view.glide.stop();
        let lines = match delta {
            MouseScrollDelta::LineDelta(_, y) => (*y * 3.0).round() as i32,
            MouseScrollDelta::PixelDelta(pos) => {
                self.wheel_remainder_px += pos.y;
                let lines = (self.wheel_remainder_px / self.char_h.max(1.0)).trunc() as i32;
                self.wheel_remainder_px -= f64::from(lines) * self.char_h.max(1.0);
                lines
            }
        };
        if lines == 0 {
            return;
        }
        if reports {
            // xterm encodes wheel up/down as virtual buttons 4/5. One report
            // per platform wheel event avoids turning high-resolution touchpad
            // deltas into an input flood; the application owns acceleration.
            self.report_mouse(if lines > 0 { 64 } else { 65 }, true, modifiers);
            *handled = true;
            return;
        }
        if !self.modes.mouse_report {
            // In the alternate screen (pager / editor) forward wheel
            // notches as arrow keys so `less`/`man` scroll naturally.
            // Honour DECCKM so apps that requested application cursor
            // keys (`vim`, `less -R`) receive the SS3 form they expect.
            let seq: &[u8] = match (lines > 0, self.modes.app_cursor_keys) {
                (true, false) => b"\x1b[A",
                (false, false) => b"\x1b[B",
                (true, true) => b"\x1bOA",
                (false, true) => b"\x1bOB",
            };
            let reps = lines.unsigned_abs() as usize;
            let mut out = Vec::new();
            for _ in 0..reps {
                out.extend_from_slice(seq);
            }
            self.write_input(&out);
        }
        *handled = true;
    }

    /// Wheel and touchpad motion over the scrollback, through the shared
    /// momentum: a notch moves three rows at once and glides on, fingers
    /// move the rows 1:1 in whole device pixels and a flick glides on.
    fn wheel_scrollback(
        &mut self,
        delta: &MouseScrollDelta,
        phase: winit::event::TouchPhase,
        scale_factor: f64,
    ) {
        let row = self.char_h.max(1.0);
        self.view.glide.set_device_scale(scale_factor);
        self.view.glide.set_view(
            self.grid_rect.height(),
            self.max_scroll() > 0,
            self.view.motion,
        );
        let step = match self
            .view
            .glide
            .wheel(delta, phase, self.view.motion.reduced())
        {
            ui::virtual_viewport::WheelStep::Scroll(step) => step,
            ui::virtual_viewport::WheelStep::Fling | ui::virtual_viewport::WheelStep::Ignored => {
                return;
            }
        };
        // A notch is three rows, not the momentum's own step.
        let step = match delta {
            MouseScrollDelta::LineDelta(_, y) => -f64::from(*y) * 3.0 * row,
            MouseScrollDelta::PixelDelta(_) => step,
        };
        let moved = self.scroll_by_px(step);
        if self.view.glide.wheel_applied(step, moved) {
            // Reviewing history moves the viewport over the scrollback and
            // nothing the terminal publishes; the text it exposes to
            // assistive technology follows with the frame's accessibility
            // refresh.
            ui::widget::note_view_only_pan();
        }
    }

    /// Where this frame shows the scrollback.
    fn grid_frame(&self) -> GridFrame {
        let grid_rect = self.grid_rect;
        let row_h = self.char_h;
        let visible_rows = (grid_rect.height() / row_h).floor() as usize;
        let start_y = self.viewport_start_y;
        // Between two rows' tops the view shows a part of one more.
        let rows = (visible_rows + usize::from(self.view.sub > 0.0))
            .min(self.grid.len().saturating_sub(start_y));
        // Past an end of the scrollback, fingers pull the rows through the
        // rubber band.
        let give = self.view.glide.give(ui::motion::frame_now());
        let top = grid_rect.y0.floor();
        GridFrame {
            start_y,
            end_y: start_y + rows,
            visible_rows,
            top,
            give,
            scroll: start_y as f64 * row_h + self.view.sub + give,
            clip: Rect::new(
                self.rect.x0,
                grid_rect.y0,
                self.scrollbar_track().x0,
                grid_rect.y0 + visible_rows as f64 * row_h,
            ),
        }
    }

    /// Paint the grid: each row of the scrollback in view, with its share of
    /// the selection and the find matches under its cells, drawn where it
    /// stands in the scrollback as if unscrolled and landed scrolled, so a
    /// row a scroll only moved paints the same and a host copies it
    /// ([`ui::scroll_copy::PaintedFrames`]); then the cursor, the bar, the
    /// bands above the grid (`outside`) and collaborators' marks.
    fn draw_grid(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        outside: Scene,
    ) {
        use ui::scroll_copy::{Painted, Piece, piece_key};
        let frame = self.grid_frame();
        let scrolled = frame.scrolled(transform);
        let mut pieces = Vec::new();
        scene.append(&outside, None);
        scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            1.0,
            transform,
            &frame.clip,
        );
        // Images sit beneath selections and glyphs: text printed over a Kitty
        // placement remains legible, and selecting cells still has the same
        // visible feedback as plain terminal content. Their row anchors are
        // buffer-absolute, so scrollback and history retention move them with
        // the corresponding rows rather than pinning them to the viewport.
        let unscrolled = frame.unscrolled(self.grid_rect, self.char_h);
        let mut images = Scene::new();
        self.draw_terminal_images(
            &mut images,
            transform,
            theme,
            unscrolled,
            frame.start_y,
            frame.end_y - frame.start_y,
        );
        if !images.encoding().is_empty() {
            scene.append(&images, Some(scrolled));
            pieces.push(Piece {
                key: piece_key("images"),
                rect: frame.on_screen(unscrolled),
                paint: Painted(images),
            });
        }
        let mut ctx = PaintCtx::new(scene, transform, text_renderer, theme);
        self.draw_rows(&mut ctx, &frame, &mut pieces);
        self.draw_cursor_piece(&mut ctx, &frame, &mut pieces);
        // Collaborators' selections and carets lie over the rows, where the
        // view shows them.
        let mut peers = Scene::new();
        let marks = transform * Affine::translate((0.0, -(self.view.sub + frame.give)));
        self.draw_peer_marks(&mut peers, marks, theme, presence);
        scene.append(&peers, None);
        scene.pop_layer();
        // A themed scrollbar indicator in the reserved right gutter, shown only
        // when the buffer overflows the viewport.
        let mut bar = Scene::new();
        self.draw_scrollbar(&mut bar, transform, theme, frame.visible_rows);
        scene.append(&bar, None);
        let fixed = vec![
            Piece {
                key: piece_key("bar"),
                rect: self.scrollbar_track(),
                paint: Painted(bar),
            },
            Piece {
                key: piece_key("peers"),
                rect: frame.clip,
                paint: Painted(peers),
            },
        ];
        let look = ui::scroll_copy::Around {
            bounds: self.rect,
            outside: Painted(outside),
        };
        match ui::scroll_copy::Showing::painted(
            vello::kurbo::Vec2::new(0.0, frame.scroll),
            frame.clip,
            ui::scroll_copy::on_screen(transform),
            look,
            pieces,
            fixed,
        ) {
            Some(shown) => self.view.shown.note(shown),
            None => self.view.shown.forget(),
        }
    }

    /// Paint the rows in view, each a damage part of the scene and a piece
    /// of the frame: its selection and find matches, then its cells.
    fn draw_rows(
        &self,
        ctx: &mut PaintCtx<'_>,
        frame: &GridFrame,
        pieces: &mut Vec<ui::scroll_copy::Piece<ui::scroll_copy::Painted>>,
    ) {
        let PaintCtx {
            scene,
            transform,
            text: text_renderer,
            theme,
        } = ctx.reborrow();
        let scrolled = frame.scrolled(transform);
        let Some(cell_style) = self.cell_style.clone() else {
            return;
        };
        let grid_rect = self.grid_rect;
        let row_h = self.char_h;
        let found: Vec<(usize, FindMatch)> = self
            .find_matches
            .iter()
            .copied()
            .enumerate()
            .filter(|(_, found)| (frame.start_y..frame.end_y).contains(&found.row))
            .collect();
        let selection_tint = theme.tokens.colors.intent.selection_tint;
        for row in frame.start_y..frame.end_y {
            let (Some(line_text), Some(attrs)) = (self.grid.get(row), self.grid_attrs.get(row))
            else {
                continue;
            };
            let py = frame.top + row as f64 * row_h;
            let mut drawn = Scene::new();
            // The selection and the find matches under the row's glyphs.
            if let Some(selection) = self.selection
                && let Some((from, to)) = selection_span_on(&selection, row, self.width)
            {
                let x0 = grid_rect.x0 + from as f64 * self.char_w;
                let x1 = grid_rect.x0 + to as f64 * self.char_w;
                let band = snap_to_device_pixels(transform, Rect::new(x0, py, x1, py + row_h));
                drawn.fill(Fill::NonZero, transform, selection_tint, None, &band);
            }
            for (index, found) in found.iter().filter(|(_, found)| found.row == row) {
                let x0 = grid_rect.x0 + found.start_col as f64 * self.char_w;
                let x1 = grid_rect.x0 + found.end_col as f64 * self.char_w;
                let marker = snap_to_device_pixels(
                    transform,
                    Rect::new(x0, py, x1.max(x0 + self.char_w), py + row_h),
                );
                let current = *index == self.find_current;
                paint_find_match(&mut drawn, transform, marker, current, theme);
            }
            let mut row_ctx = PaintCtx::new(&mut drawn, transform, &mut *text_renderer, theme);
            self.draw_cell_row(&mut row_ctx, &cell_style, line_text, attrs, py);
            // Each row on screen is a damage part: typing repaints its row.
            // Glyphs may overhang their row by up to a row above and below,
            // and the widget's width by up to two cells.
            let band = frame.on_screen(Rect::new(
                self.rect.x0 - 2.0 * self.char_w,
                py - row_h,
                self.rect.x1 + 2.0 * self.char_w,
                py + 2.0 * row_h,
            ));
            let key = ui::scene_parts::PartKey::new("terminal.row", row - frame.start_y);
            let bounds = vec![transform.transform_rect_bbox(band.intersect(frame.clip))];
            ui::scene_parts::part(scene, key, bounds, |scene| {
                scene.append(&drawn, Some(scrolled));
            });
            pieces.push(ui::scroll_copy::Piece {
                key: ui::scroll_copy::piece_key(row),
                rect: band,
                paint: ui::scroll_copy::Painted(drawn),
            });
        }
    }

    /// The cursor, a damage part of its own: moving it repaints the cell it
    /// left and the cell it entered.
    fn draw_cursor_piece(
        &self,
        ctx: &mut PaintCtx<'_>,
        frame: &GridFrame,
        pieces: &mut Vec<ui::scroll_copy::Piece<ui::scroll_copy::Painted>>,
    ) {
        let PaintCtx {
            scene,
            transform,
            text: text_renderer,
            theme,
        } = ctx.reborrow();
        let Some(cell_style) = self.cell_style.clone() else {
            return;
        };
        let mut cursor = Scene::new();
        let cursor_at = self.draw_block_cursor(
            &mut PaintCtx::new(&mut cursor, transform, text_renderer, theme),
            &cell_style,
            (frame.start_y, frame.end_y - frame.start_y),
            frame.top + frame.start_y as f64 * self.char_h,
        );
        let bounds = cursor_at
            .map(|rect| {
                vec![transform.transform_rect_bbox(frame.on_screen(rect).intersect(frame.clip))]
            })
            .unwrap_or_default();
        let scrolled = frame.scrolled(transform);
        let key = ui::scene_parts::PartKey::new("terminal.cursor", ());
        ui::scene_parts::part(scene, key, bounds, |scene| {
            scene.append(&cursor, Some(scrolled));
        });
        if let Some(rect) = cursor_at {
            pieces.push(ui::scroll_copy::Piece {
                key: ui::scroll_copy::piece_key("cursor"),
                rect: frame.on_screen(rect),
                paint: ui::scroll_copy::Painted(cursor),
            });
        }
    }

    /// Paint the block cursor, pinned to the live output — while the user
    /// has scrolled back into history it hides (matching xterm/kitty), so
    /// it doesn't imply the caret is somewhere on an old line. `window` is
    /// the first row in view and how many are, `top` where the first of
    /// them is drawn. Returns where it may paint (its glyph may overhang the
    /// cell by up to a cell on every side), `None` when it is hidden.
    fn draw_block_cursor(
        &self,
        ctx: &mut PaintCtx<'_>,
        cell_style: &TextStyle,
        window: (usize, usize),
        top: f64,
    ) -> Option<Rect> {
        let (start_y, visible_rows) = window;
        let PaintCtx {
            scene,
            transform,
            text: text_renderer,
            ..
        } = ctx.reborrow();
        let palette = &self.palette;
        let grid_rect = self.grid_rect;
        let scroll = self.scroll_offset;
        let cursor_color = self.last_cursor_color;
        // Absolute grid row of the cursor → visible row (may be scrolled off).
        let cursor_abs = self.grid.len().saturating_sub(self.height) + self.cursor_y;
        let scrolled_back = !self.alt_active && scroll > 0;
        let shown = self.modes.cursor_visible
            && !scrolled_back
            && cursor_abs >= start_y
            && cursor_abs < start_y + visible_rows.max(1);
        if !shown {
            return None;
        }
        let cy_local = cursor_abs.saturating_sub(start_y);
        let cx = grid_rect.x0 + self.cursor_x as f64 * self.char_w;
        let cy = (top + cy_local as f64 * self.char_h).floor();
        // An unmistakable, theme-coloured "inverted cell" cursor.
        let cursor_rect = snap_to_device_pixels(
            transform,
            Rect::new(cx, cy, cx + self.char_w, cy + self.char_h),
        );
        scene.fill(Fill::NonZero, transform, cursor_color, None, &cursor_rect);

        // Re-punch the glyph beneath the block in the background color so
        // the character under the caret stays legible (a solid block that
        // hides its own cell reads as a blank cursor). This is the standard
        // "inverted cell" block cursor.
        if let Some(ch) = self
            .grid
            .get(cursor_abs)
            .and_then(|line| cell_char_at(line, self.cursor_x))
            && !ch.is_whitespace()
        {
            text_renderer.draw_style(
                scene,
                &ch.to_string(),
                palette.bg_default,
                transform * Affine::translate((cx, cy)),
                None,
                cell_style,
            );
        }
        Some(Rect::new(
            cx - self.char_w,
            cy - self.char_h,
            cx + 2.0 * self.char_w,
            cy + 2.0 * self.char_h,
        ))
    }
}

/// Where a frame shows the scrollback: rows `start_y..end_y` of it, the
/// first `top` plus its place in the scrollback down the grid unscrolled,
/// landed `scroll` higher, in `clip`.
struct GridFrame {
    start_y: usize,
    end_y: usize,
    visible_rows: usize,
    top: f64,
    /// The rubber band's give past an end, part of `scroll`.
    give: f64,
    scroll: f64,
    clip: Rect,
}

impl GridFrame {
    /// The transform that lands what was drawn unscrolled under
    /// `transform` where the view shows it.
    fn scrolled(&self, transform: Affine) -> Affine {
        transform * Affine::translate((0.0, -self.scroll)) * transform.inverse()
    }

    /// Where `rect`, drawn unscrolled, lands on screen.
    fn on_screen(&self, rect: Rect) -> Rect {
        rect - vello::kurbo::Vec2::new(0.0, self.scroll)
    }

    /// The grid as if its first row were the scrollback's: what draws a row
    /// at its place in the view draws it at its place in the scrollback.
    fn unscrolled(&self, grid_rect: Rect, row_h: f64) -> Rect {
        Rect::new(
            grid_rect.x0,
            self.top + self.start_y as f64 * row_h,
            grid_rect.x1,
            self.top + self.end_y as f64 * row_h,
        )
    }
}
