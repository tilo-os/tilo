// Included into `app.rs` (shares its module + imports), beside
// `app_events.rs`: the wheel, and which pointer moves leave a frame free to
// scroll by copying what stays on screen.

impl PdfApp {
    /// Whether the pointer moving from `from` to `to` changes nothing drawn:
    /// both on the page canvas, clear of everything there that shows hover
    /// (the page bubble's reveal band, the selection bar), with nothing
    /// floating over the canvas (a menu, a panel in a narrow window). A
    /// finger
    /// scrolling the page moves the pointer every frame; such a frame only
    /// scrolled, and repaints only what scrolled in.
    fn pointer_move_is_quiet(&self, from: Point, to: Point) -> bool {
        // A move that went nowhere (a finger dragging the page reports
        // where it went down) changes no hover, wherever it is.
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

    /// `WindowEvent::MouseWheel`: the page rail / sidebar scroll it under the
    /// pointer, Ctrl+wheel zooms, otherwise it pans the canvas.
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

        // Wheel over the page rail scrolls its thumbnail list.
        if self.show_rail && self.rail_rect.contains(p) {
            if self.page_sidebar.on_wheel(-raw_y) {
                self.dirty = true;
            }
            *handled = true;
            return;
        }

        // Wheel over the sidebar scrolls its list.
        if self.show_sidebar && self.sidebar_rect.contains(p) {
            // (sidebar_max_scroll needs the text renderer, so just
            // nudge and let the next draw clamp it.)
            self.sidebar_scroll = (self.sidebar_scroll - raw_y).max(0.0);
            self.dirty = true;
            *handled = true;
            return;
        }

        if ctrl {
            // Ctrl+wheel zooms, centred on the cursor.
            let factor = if raw_y > 0.0 {
                1.0 + raw_y / 240.0
            } else {
                1.0 / (1.0 - raw_y / 240.0)
            };
            self.zoom_at(factor, p);
            *handled = true;
            return;
        }

        // Shift+wheel (or a horizontal wheel) pans horizontally;
        // otherwise vertical. Either axis only moves if it can. The
        // clamp lives in the shared kernel's `pan_by`.
        //
        // Fingers (a touchpad, a touchscreen) move the page 1:1 and, lifted
        // while moving, leave it gliding to a stop, as every Tilo scroll
        // surface does (the shared `ScrollMomentum`); a wheel's notches
        // move it at once.
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
        // Only the view moved: the frame can scroll what is on screen
        // (`collect_damage`) instead of repainting the window.
        self.scrolled = true;
        // A selection being dragged follows the text the wheel brought
        // under the pointer.
        if self.selection.is_some_and(|sel| sel.dragging) {
            self.update_selection_at(p);
        }
        *handled = true;
    }
}
