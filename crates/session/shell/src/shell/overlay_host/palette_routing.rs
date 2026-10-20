//! Palette event routing and the fallthrough to the content.

#[allow(
    clippy::wildcard_imports,
    reason = "private implementation module shares its enclosing host types"
)]
use super::*;

impl ShellOverlayHost {
    /// What the palette asked of its host while handling an event: a
    /// confirmed field, a step backed out of, a scope token taken away.
    pub(super) fn take_palette_requests(&mut self, doc: &mut ui::widget::Edits<'_>, root: &ObjId) {
        if let Some(text) = self.palette.take_submitted_argument() {
            self.submit_param_field(text, doc, root);
        }
        if self.palette.take_argument_cancelled() {
            self.palette_step_back();
        }
        if self.palette.take_scope_token_removed() {
            self.widen_search_scope();
        }
    }

    /// A header row is a filter: a click on "Files" shows only files.
    pub(super) fn filter_by_clicked_header(
        &mut self,
        event: &WindowEvent,
        doc: &automerge::AutoCommit,
        root: &ObjId,
    ) -> bool {
        let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: winit::event::MouseButton::Left,
            ..
        } = event
        else {
            return false;
        };
        let Some(group) = self.last_cursor.and_then(|p| self.palette.header_at(p)) else {
            return false;
        };
        if self.param_flow.is_some() || self.secondary_flow.is_some() {
            return false;
        }
        if let Some(kind) = self
            .present_kinds
            .iter()
            .find(|kind| kind.localized_label() == group)
            .cloned()
        {
            self.active_kind_filter = vec![kind];
            self.last_search_query.clear(); // force a refresh
            self.refresh_search_results(doc, root);
        }
        true
    }

    /// A press at `p` on the search filters: "All", "More" or one kind's
    /// chip. Returns whether it hit one.
    fn click_palette_filter(
        &mut self,
        p: vello::kurbo::Point,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        // A direct way to reach "nothing selected", not just
        // untick every chip one at a time. Targets are the pointer
        // rects (`selector_hit_rect`), the same ones the semantic
        // nodes publish.
        let all_hit = self.search_filters.search_all_chip_hit_rect();
        if all_hit.area() > 0.0 && all_hit.contains(p) {
            if !self.active_kind_filter.is_empty() {
                self.active_kind_filter.clear();
                self.last_search_query.clear(); // force a refresh
                self.refresh_search_results(&ctx.state, ctx.path);
            }
            return true;
        }
        let more_hit = self.search_filters.search_more_hit_rect();
        if more_hit.area() > 0.0 && more_hit.contains(p) {
            self.search_filters.more_open = !self.search_filters.more_open;
            return true;
        }
        let kinds: Vec<_> = self.search_filters.kinds().cloned().collect();
        for idx in 0..self.search_filters.search_chip_rects.len() {
            if self.search_filters.search_chip_hit_rect(idx).contains(p) {
                if let Some(kind) = kinds.get(idx).cloned() {
                    toggle_kind(&mut self.active_kind_filter, kind);
                    self.last_search_query.clear(); // force a refresh
                    self.refresh_search_results(&ctx.state, ctx.path);
                }
                return true;
            }
        }
        false
    }

    /// Palette-owned input, ahead of the switcher/content dispatch: kind
    /// chips, a click on a result row, keyboard Enter, then everything
    /// else the palette widget itself wants (typing, arrow selection).
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
        // Before the row hit-test below so a chip click doesn't fall
        // through to the top result.
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: winit::event::MouseButton::Left,
            ..
        } = event
            && let Some(p) = self.last_cursor
            && self.click_palette_filter(p, ctx)
        {
            *ctx.handled = true;
            return true;
        }
        // The selected row previews what a held modifier would do.
        let held = ctx.modifiers.state();
        self.palette.set_held_modifiers(held);
        let modifier = ui::widget::command_palette::ActivationModifier::of(held);
        if self.filter_by_clicked_header(event, &ctx.state, ctx.path) {
            *ctx.handled = true;
            return true;
        }
        // Before delegating to the palette, so a single click both
        // selects and fires.
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: winit::event::MouseButton::Left,
            ..
        } = event
            && let Some(p) = self.last_cursor
            && let Some(row) = self.palette.row_at(p)
        {
            self.palette.set_cursor(row);
            self.activate_selected_result_with(&mut ctx.state, ctx.path, modifier);
            *ctx.handled = true;
            return true;
        }
        // One activation per press. A held Enter must not repeat a failed
        // invocation while its recovery context stays open. A field step
        // takes its own Enter, below.
        if let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    logical_key: Key::Named(NamedKey::Enter),
                    state: ElementState::Pressed,
                    repeat,
                    ..
                },
            ..
        } = event
            && !self.palette.in_argument_mode()
        {
            // Nothing is listed while the field stands alone.
            if !repeat && !self.palette.shows_field_only() {
                self.activate_selected_result_with(&mut ctx.state, ctx.path, modifier);
            }
            *ctx.handled = true;
            return true;
        }
        // Let the palette own the character/backspace/arrow keys.
        if let Some(palette_child) = palette_child {
            let query_before = self.palette.query().to_string();
            self.palette
                .handle_event(event, &mut ctx.reborrow(ctx.path, palette_child));
            self.take_palette_requests(&mut ctx.state, ctx.path);
            if let Some((id, delta)) = self.palette.take_adjustment() {
                self.step_result(&id, delta, &mut ctx.state, ctx.path);
            }
            // Recompute results when a character/backspace changes the query.
            if self.palette.query() != query_before
                && self.param_flow.is_none()
                && self.secondary_flow.is_none()
            {
                self.refresh_search_results(&ctx.state, ctx.path);
            }
            if *ctx.handled {
                return true;
            }
        }
        false
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
                self.palette
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
        if let WindowEvent::MouseWheel { delta, .. } = event {
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
        if let Some(&content_child) = children.first() {
            self.content
                .handle_event(event, &mut ctx.reborrow(ctx.path, content_child));
            // A visible tab/maximized close button begins its animation in
            // the widget above. Commit that exact tab before this input
            // dispatch returns so no revision or crash can restore it.
            let _ = self.harvest_pending_tab_closes();
            // A right-click on a tab pill has just been recorded by its bar;
            // open the menu in the same dispatch so it is on screen — and
            // in the accessibility tree — by the time this event returns.
            self.harvest_pending_tab_context_menus();
            // So is a popover the bar asked for.
            self.harvest_bar_popover_request();
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
