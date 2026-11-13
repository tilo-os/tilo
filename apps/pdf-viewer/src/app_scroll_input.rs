impl PdfApp {
    fn pointer_move_is_quiet(&self, from: Point, to: Point) -> bool {
        if from == to {
            return true;
        }
        let canvas = self.canvas_rect;
        let floating = self.context_menu.is_some()
            || (self.overlay_mode && (self.show_rail || self.show_sidebar));
        self.has_document
            && !floating
            && canvas.contains(from)
            && canvas.contains(to)
            && !self
                .canvas_hover_zones
                .iter()
                .any(|zone| zone.contains(from) || zone.contains(to))
    }

    fn on_mouse_wheel(
        &mut self,
        delta: &MouseScrollDelta,
        phase: winit::event::TouchPhase,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        handled: &mut bool,
    ) {
        let p = self.last_mouse;
        let (raw_x, raw_y) = match delta {
            MouseScrollDelta::LineDelta(x, y) => (f64::from(*x) * 56.0, f64::from(*y) * 56.0),
            MouseScrollDelta::PixelDelta(pos) => (pos.x / scale_factor, pos.y / scale_factor),
        };
        let mods = modifiers.state();
        let ctrl = mods.control_key() || mods.super_key();
        tracing::debug!(
            kind = match delta {
                MouseScrollDelta::LineDelta(..) => "line",
                MouseScrollDelta::PixelDelta(..) => "pixel",
            },
            raw_x,
            raw_y,
            ctrl,
            shift = mods.shift_key(),
            "pdf MouseWheel"
        );

        if self.show_rail && self.rail_rect.contains(p) {
            if self.page_sidebar.on_wheel(-raw_y) {
                self.dirty = true;
            }
            *handled = true;
            return;
        }

        if self.show_sidebar && self.sidebar_rect.contains(p) {
            self.sidebar_scroll = (self.sidebar_scroll - raw_y).max(0.0);
            self.dirty = true;
            *handled = true;
            return;
        }

        if ctrl {
            let factor = if raw_y > 0.0 {
                1.0 + raw_y / 240.0
            } else {
                1.0 / (1.0 - raw_y / 240.0)
            };
            self.zoom_at(factor, p);
            *handled = true;
            return;
        }

        if mods.shift_key() && self.is_overflowing_x() {
            self.glide.stop();
            self.viewport.pan_by(-raw_y, 0.0);
        } else if matches!(delta, MouseScrollDelta::PixelDelta(_)) {
            let step = self.glide.wheel(delta, phase, self.reduce_motion);
            let before = self.scroll_y();
            let dy = if matches!(step, ui::WheelStep::Scroll(_)) {
                -raw_y
            } else {
                0.0
            };
            self.viewport.pan_by(-raw_x, dy);
            if matches!(step, ui::WheelStep::Scroll(_)) {
                self.glide.wheel_applied(dy, self.scroll_y() - before);
            }
        } else {
            self.glide.stop();
            self.viewport.pan_by(-raw_x, -raw_y);
        }
        self.current_page = self.nearest_page();
        self.scrolled = true;
        if self.selection.is_some_and(|sel| sel.dragging) {
            self.update_selection_at(p);
        }
        *handled = true;
    }
}
