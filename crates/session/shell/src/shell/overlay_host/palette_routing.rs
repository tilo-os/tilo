//! Search's event routing and the fallthrough to the content.

#[allow(
    clippy::wildcard_imports,
    reason = "private implementation module shares its enclosing host types"
)]
use super::*;

impl ShellOverlayHost {
    /// Search's input, ahead of the switcher/content dispatch: the
    /// launcher takes it itself (its field, its chips, its rows), and what
    /// it asks is answered before the event returns.
    pub(super) fn route_palette_event(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
        palette_child: Option<NodeId>,
    ) -> bool {
        if !self.palette_visible {
            return false;
        }
        if matches!(event, WindowEvent::KeyboardInput { .. }) {
            crate::compositor::key_latency::note_key_handler(
                crate::compositor::key_latency::KeyHandler::Surface("palette"),
            );
        }
        self.open_launcher_if_asked();
        let (Some(launcher), Some(palette_child)) = (self.launcher.as_mut(), palette_child) else {
            return false;
        };
        // Open search holds the keyboard: the launcher's field types.
        if !launcher.app.is_focused() {
            launcher.app.set_focus(true);
        }
        launcher
            .app
            .handle_event(event, &mut ctx.reborrow(ctx.path, palette_child));
        // A keystroke's rows, a choice's run.
        self.serve_launcher(&mut ctx.state, ctx.path);
        if matches!(
            event,
            WindowEvent::KeyboardInput { .. } | WindowEvent::Ime(_)
        ) {
            *ctx.handled = true;
        }
        *ctx.handled
    }

    /// ←/→ on a value row: step it in place and show the new value, or say
    /// on the row why it could not.
    pub(super) fn step_result(
        &mut self,
        id: &str,
        delta: i8,
        doc: &mut ui::widget::Edits<'_>,
        root: &ObjId,
    ) {
        let Some(result) = id
            .strip_prefix("result:")
            .and_then(|at| at.parse::<usize>().ok())
            .and_then(|at| self.search_results.get(at))
            .cloned()
        else {
            return;
        };
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        match self
            .search_registry
            .adjust(&result, delta, doc, root, &chrome)
        {
            Ok(()) => {
                // Never equal to a real query: the next refresh rebuilds the
                // row with the value it now has.
                self.last_search_query = "\0stepped\0".into();
                self.refresh_search_results(doc, root);
            }
            Err(error) => {
                self.search
                    .set_selected_status(t("host-action-failed-status"), error.user_message());
            }
        }
        self.palette_layout_dirty = true;
    }

    /// The tail of `handle_event`: the strip switcher, click-to-focus,
    /// horizontal strip navigation, then content dispatch. A horizontal
    /// gesture steps focus along the strip (the same step Super+Left/Right
    /// takes); 0.5 rejects trackpad noise.
    pub(super) fn dispatch_pointer_and_content_event(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
        children: &[NodeId],
    ) {
        // First dibs: the open strip switcher floats over the windows, and
        // the wheel over the ribbon or its labels moves between strips.
        if self.route_strip_switcher_pointer(event) || self.route_strip_switcher_wheel(event) {
            *ctx.handled = true;
            return;
        }
        // Empty canvas is the canvas's: the caret, a band, panning and
        // zooming, and what is typed at the caret.
        if self.route_canvas_event(event, *ctx.modifiers, ctx.scale_factor) {
            *ctx.handled = true;
            return;
        }
        // Click-anywhere-in-a-group focuses that group: hit-test before
        // forwarding and stamp `tree.active_strip`/`active_group`, or
        // clicking the window body (not a tab pill) never updates them.
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: winit::event::MouseButton::Left,
            ..
        } = event
            && let Some(p) = self.last_cursor
        {
            self.stamp_active_group_from_click(p);
        }
        // Horizontal strip navigation is shell chrome, so it must run before
        // the active app can consume a generic wheel event. Vertical and
        // diagonally vertical gestures still belong to app content.
        if let WindowEvent::MouseWheel { delta, .. } = event
            && !self.canvas_shown()
        {
            let (dx, dy) = match *delta {
                winit::event::MouseScrollDelta::LineDelta(x, y) => (x, y),
                winit::event::MouseScrollDelta::PixelDelta(p) => {
                    (f32_of_f64(p.x) / 40.0, f32_of_f64(p.y) / 40.0)
                }
            };
            // Winit's deltas are negative for a wheel turned (or fingers
            // scrolling) rightward, which scrolls content toward its later
            // columns: forward, as Super+Right.
            if dx.abs() > 0.5 && dx.abs() > dy.abs() {
                let dir = if dx < 0.0 { 1 } else { -1 };
                if self.step_focus_along_strip(dir) {
                    *ctx.handled = true;
                    return;
                }
            }
        }
        // The top bar stands over the desktop's own content: the pointer on
        // a bar, and the keys while it holds the keyboard, are the bar's.
        if self.route_topbar_event(event, &mut ctx.reborrow(ctx.path, ctx.node_id)) {
            return;
        }
        if let Some(&content_child) = children.first() {
            self.content
                .handle_event(event, &mut ctx.reborrow(ctx.path, content_child));
            // A visible tab/maximized close button begins its animation in
            // the widget above. Commit that exact tab before this input
            // dispatch returns so no revision or crash can restore it.
            let _ = self.harvest_pending_tab_closes();
            // A window carried on the canvas is kept where it was let go,
            // before the next sync could put it back.
            self.harvest_pending_canvas_moves();
            // A right-click on a tab pill has just been recorded by its bar;
            // open the menu in the same dispatch so it is on screen — and
            // in the accessibility tree — by the time this event returns.
            self.harvest_pending_tab_context_menus();
            // `/` the app did not type opens search of that app, as Ctrl+K
            // does. A focused field or terminal takes `/` as text.
            if !*ctx.handled
                && !self.palette_visible
                && is_plain_slash(event, ctx.modifiers.state())
                && self.content.ime_cursor_rect().is_none()
            {
                self.open_palette_with_scope(crate::shell::chrome_state::SearchScope::ActiveApp);
                *ctx.handled = true;
            }
        }
    }
}
