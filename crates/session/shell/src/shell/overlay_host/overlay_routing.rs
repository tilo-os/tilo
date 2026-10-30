//! Event routing through the modal, absorbing and pointer-capturing overlays.

#[allow(
    clippy::wildcard_imports,
    reason = "private implementation module shares its enclosing host types"
)]
use super::*;

impl ShellOverlayHost {
    /// Tracks the last cursor position and updates strip ribbon/rail
    /// hover, for hit-testing overlay clicks in the absence of position
    /// data on `MouseInput`. First stage of `handle_event`.
    pub(super) fn track_cursor_position(&mut self, event: &WindowEvent, scale_factor: f64) {
        // A pointer that left the window is nowhere a peer can be shown. Only
        // the shared desk forgets it: `last_cursor` still hit-tests.
        if matches!(event, WindowEvent::CursorLeft { .. }) {
            self.desk_pointer_left(ui::time::now());
            self.strip_switcher_pointer_left();
            if let Some(desk) = &self.shared_desk {
                desk.set_pointer(None);
            }
        }
        // Focus loss or a fresh press ends an abandoned navigation gesture.
        // Escape alone keeps the matching release owned across page changes.
        // A finger put down on another display works there, as the pointer
        // arriving there does: an edge swipe or a tap the shell keeps for
        // itself never becomes pointer motion, and what it opens belongs
        // on the display it was made on.
        if let WindowEvent::Touch(touch) = event
            && touch.phase == winit::event::TouchPhase::Started
        {
            self.focus_display_at(Point::new(
                touch.location.x / scale_factor,
                touch.location.y / scale_factor,
            ));
        }
        if let WindowEvent::MouseInput {
            state: winit::event::ElementState::Pressed,
            ..
        } = event
        {
            // Acting on a button ends its tooltip.
            self.bar_tooltip.dismiss();
        }
        if let WindowEvent::CursorMoved { position, .. } = event {
            let p = Point::new(position.x / scale_factor, position.y / scale_factor);
            self.last_cursor = Some(p);
            self.bar_tooltip_pointer(p);
            if let Some(desk) = &self.shared_desk {
                desk.set_pointer(Some((p.x, p.y)));
            }
            self.desk_pointer(p, ui::time::now());
            self.focus_display_at(p);
            // A ribbon or rail segment thickens under the pointer to say
            // it's clickable, and the labels read its path.
            self.strip_switcher_pointer_moved(p);
        }
    }

    /// Say which sheet an Enter press meets, so a join typed into the
    /// directory sheet that never left shows where its Enter went.
    fn trace_enter_at_sheets(&self, event: &WindowEvent) {
        if let WindowEvent::KeyboardInput {
            event:
                winit::event::KeyEvent {
                    logical_key: winit::keyboard::Key::Named(winit::keyboard::NamedKey::Enter),
                    state: winit::event::ElementState::Pressed,
                    ..
                },
            ..
        } = event
            && (self.directory_sheet.is_open() || self.peer_sync_overlay.visible())
        {
            tracing::info!(
                directory_sheet = self.directory_sheet.is_open(),
                join_card = self.peer_sync_overlay.visible(),
                app_sheet = self.app_sheet.is_some(),
                "Enter reached the shell's join sheets"
            );
        }
    }

    /// Modal overlays that absorb an event outright before anything else
    /// sees it: the file chooser (for the window that asked; a D-Bus call
    /// is parked on the answer), an in-flight peer-sync token prompt, and
    /// the projection sheet.
    pub(super) fn route_modal_absorbing_overlays(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        // Runs before everything: the sheet takes what is meant for the
        // window that asked (a D-Bus method in another process is parked on
        // the answer, and a typed filename shares characters with chords)
        // and leaves the rest of the desktop alone.
        self.focus_asking_window_on_press(event);
        self.trace_enter_at_sheets(event);
        let place = self.file_picker_place();
        if self
            .file_picker_overlay
            .handle_event(place, self.last_cursor, event, ctx)
        {
            return true;
        }

        // Before every other overlay: a token prompt owns the keyboard
        // outright, since a token shares characters with chords.
        if self.peer_sync_overlay.visible() {
            self.handle_peer_sync_event(event, ctx);
            return true;
        }
        if self.app_sheet.is_some() {
            self.handle_app_sheet_event(event, ctx);
            return true;
        }
        // So does the directory sheet, whose search takes pasted links.
        if self.route_directory(event, ctx) {
            return true;
        }

        if self.route_layer_band(Band::Modal, event, ctx) {
            return true;
        }

        if self.route_display_sheet(event, ctx) {
            return true;
        }

        false
    }

    /// A drag along the now-playing card's playhead keeps the pointer
    /// until it is let go.
    pub(super) fn route_now_playing_scrub(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
    ) -> bool {
        if !self.now_playing_card.scrubbing() {
            return false;
        }
        match event {
            // A finger that landed on the playhead scrubs it as the held
            // pointer does: it follows the finger, and lifting it seeks.
            WindowEvent::Touch(touch) => {
                match touch.phase {
                    winit::event::TouchPhase::Started => {}
                    winit::event::TouchPhase::Moved => self.drag_now_playing_seek(Point::new(
                        touch.location.x / scale_factor,
                        touch.location.y / scale_factor,
                    )),
                    winit::event::TouchPhase::Ended => self.release_now_playing_seek(),
                    // Taken away (a gesture claimed it): nothing seeks.
                    winit::event::TouchPhase::Cancelled => {
                        self.now_playing_card.reset();
                        self.bump_chrome();
                    }
                }
                true
            }
            WindowEvent::CursorMoved { .. } => {
                if let Some(point) = self.last_cursor {
                    self.drag_now_playing_seek(point);
                }
                true
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: winit::event::MouseButton::Left,
                ..
            } => {
                self.release_now_playing_seek();
                true
            }
            _ => false,
        }
    }

    pub(super) fn is_pointer_event(event: &WindowEvent) -> bool {
        matches!(
            event,
            WindowEvent::CursorMoved { .. }
                | WindowEvent::MouseInput { .. }
                | WindowEvent::MouseWheel { .. }
                | WindowEvent::Touch(_)
        )
    }

    /// A key on a portal sheet that takes no typed text: move between its
    /// rows, page its review, confirm, cancel or edit the row in focus.
    fn portal_dialog_key(
        &mut self,
        logical_key: &Key,
        physical_key: winit::keyboard::PhysicalKey,
        modifiers: winit::event::Modifiers,
    ) {
        match logical_key {
            Key::Named(NamedKey::ArrowDown | NamedKey::Tab) => {
                self.portal_dialogs.select_next(false);
            }
            Key::Named(NamedKey::ArrowUp) => {
                self.portal_dialogs.select_next(true);
            }
            Key::Named(NamedKey::ArrowLeft | NamedKey::PageUp)
                if self.portal_dialogs.review_pages().is_some() =>
            {
                self.portal_dialogs.review_back();
            }
            Key::Named(NamedKey::ArrowRight | NamedKey::PageDown)
                if self.portal_dialogs.reviewing_more() =>
            {
                self.portal_dialogs.confirm();
            }
            Key::Named(NamedKey::Enter) => {
                self.portal_dialogs.confirm();
            }
            Key::Named(NamedKey::Escape) => {
                self.portal_dialogs.cancel();
            }
            Key::Named(NamedKey::Backspace | NamedKey::Delete) => {
                if !self.portal_dialogs.edit_launcher_name(logical_key) {
                    self.portal_dialogs.clear_selected_shortcut();
                }
            }
            Key::Named(NamedKey::Space) if self.portal_dialogs.is_usb_device_selector() => {
                self.portal_dialogs.toggle_selected_usb_device();
            }
            Key::Named(NamedKey::Space) if self.portal_dialogs.remember_label().is_some() => {
                self.portal_dialogs.toggle_remember();
            }
            _ => {
                if !self.portal_dialogs.edit_launcher_name(logical_key) {
                    self.portal_dialogs.key(&physical_key, modifiers);
                }
            }
        }
    }

    /// A key on a confirmation showing its draft. Every key is the sheet's:
    /// Tab moves between the fields and the answers, Space ticks a check
    /// row or chooses an answer, Enter answers (a new line in a prose
    /// field, where Ctrl+Enter answers), Page Down and Up turn the pages of
    /// a long draft, and everything else is typing in the focused field.
    fn portal_draft_key(
        &mut self,
        event: &WindowEvent,
        logical_key: &Key,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) {
        let state = ctx.modifiers.state();
        match logical_key {
            Key::Named(NamedKey::Tab) => {
                self.portal_dialogs.select_next(state.shift_key());
            }
            Key::Named(NamedKey::Escape) => {
                self.portal_dialogs.cancel();
            }
            Key::Named(NamedKey::Enter)
                if state.control_key() || !self.portal_dialogs.draft_takes_new_line() =>
            {
                self.portal_dialogs.confirm();
            }
            Key::Named(NamedKey::PageDown) if self.portal_dialogs.reviewing_more() => {
                self.portal_dialogs.confirm();
            }
            Key::Named(NamedKey::PageUp) => {
                self.portal_dialogs.review_back();
            }
            Key::Named(NamedKey::Space) if self.portal_dialogs.toggle_draft_focus() => {}
            _ => {
                self.portal_dialogs.edit_secret(event, ctx);
            }
        }
    }

    /// Portal sheets are modal: the keyboard and pointer remain in shell
    /// chrome until the person explicitly confirms or cancels.
    pub(super) fn route_portal_dialog_event(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        if self.portal_dialogs.view().is_none() {
            return false;
        }
        if self.portal_dialogs.prompt().is_some() {
            // The password prompt is modal and routes its own input: keys,
            // the input method, the pointer and touch. A press is placed
            // where the pointer last was, which may have been over a window
            // whose motion never reached this tree.
            if let (WindowEvent::MouseInput { .. }, Some(point)) = (event, self.last_cursor) {
                let moved = ui::synthetic_input::cursor_moved(
                    point.x * ctx.scale_factor,
                    point.y * ctx.scale_factor,
                );
                self.portal_dialogs.prompt_event(&moved, ctx);
            }
            self.portal_dialogs.prompt_event(event, ctx);
            *ctx.handled = true;
            return true;
        }
        if self.portal_dialogs.layout(self.overlay_area()).is_none() {
            // A security review that cannot fit is not an invisible Allow
            // button. Keep Escape available, but consume every other input.
            if matches!(
                event,
                WindowEvent::KeyboardInput {
                    event: KeyEvent {
                        state: ElementState::Pressed,
                        logical_key: Key::Named(NamedKey::Escape),
                        ..
                    },
                    ..
                }
            ) {
                self.portal_dialogs.cancel();
            }
            return true;
        }
        let modifiers = *ctx.modifiers;
        match event {
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        state: ElementState::Pressed,
                        logical_key,
                        physical_key,
                        repeat,
                        ..
                    },
                ..
            } => {
                // Holding Enter must not traverse the disclosure and approve
                // it with the same physical key press.
                let answers = matches!(
                    logical_key,
                    Key::Named(NamedKey::Enter | NamedKey::PageDown)
                );
                if *repeat
                    && (self.portal_dialogs.review_pages().is_some()
                        || (answers && self.portal_dialogs.is_draft_sheet()))
                {
                    *ctx.handled = true;
                    return true;
                }
                let state = modifiers.state();
                let capture_chord = self.portal_dialogs.is_shortcut_editor()
                    && (state.control_key()
                        || state.alt_key()
                        || state.shift_key()
                        || state.super_key());
                if self.portal_dialogs.is_draft_sheet() {
                    self.portal_draft_key(event, logical_key, ctx);
                } else if self.portal_dialogs.is_text_entry() {
                    // Every key belongs to a sheet that takes typed text
                    // while it is up; none reaches an application.
                    match logical_key {
                        Key::Named(NamedKey::Enter) => {
                            self.portal_dialogs.confirm();
                        }
                        Key::Named(NamedKey::Escape) => {
                            self.portal_dialogs.cancel();
                        }
                        Key::Named(NamedKey::ArrowDown | NamedKey::Tab) => {
                            self.portal_dialogs.select_next(false);
                        }
                        Key::Named(NamedKey::ArrowUp) => {
                            self.portal_dialogs.select_next(true);
                        }
                        _ => {
                            self.portal_dialogs.edit_secret(event, ctx);
                        }
                    }
                } else if capture_chord {
                    self.portal_dialogs.key(physical_key, modifiers);
                } else {
                    self.portal_dialog_key(logical_key, *physical_key, modifiers);
                }
            }
            WindowEvent::Ime(_) if self.portal_dialogs.is_text_entry() => {
                self.portal_dialogs.edit_secret(event, ctx);
            }
            WindowEvent::CursorMoved { .. }
            | WindowEvent::MouseInput {
                button: winit::event::MouseButton::Left,
                ..
            } => self.route_portal_dialog_pointer(event, ctx),
            _ => {}
        }
        *ctx.handled = true;
        true
    }

    /// Pointer input on the sheet: a press on a text field (its show/hide
    /// control included) is the field's and anywhere else the sheet's;
    /// movement and releases reach the fields for hover and that control.
    pub(super) fn route_portal_dialog_pointer(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) {
        let Some(layout) = self.portal_dialogs.layout(self.overlay_area()) else {
            return;
        };
        if let (
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                ..
            },
            Some(point),
        ) = (event, self.last_cursor)
        {
            // The field pressed takes the keyboard before the press places
            // its caret.
            self.portal_dialogs.focus_draft_at(point, &layout);
        }
        let used = self.portal_dialogs.field_pointer_event(event, ctx, &layout);
        if !used
            && let (
                WindowEvent::MouseInput {
                    state: ElementState::Pressed,
                    ..
                },
                Some(point),
            ) = (event, self.last_cursor)
        {
            self.portal_dialogs.click(point, &layout);
        }
    }

    pub(super) fn notification_history_covers_pointer(&self, point: Point) -> bool {
        self.overlay_flags().quick_settings
            && matches!(
                self.quick_settings.showing().page,
                tilo_quick_settings::Page::Notifications | tilo_quick_settings::Page::Notification
            )
            && self.quick_settings_rect().contains(point)
    }

    /// Banners own completed clicks before ordinary overlay absorption.
    /// Foreground history cancels covered gestures and keeps its own presses.
    pub(super) fn route_absorbable_pointer_and_toast_click(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        let covered = self
            .last_cursor
            .is_some_and(|point| self.notification_history_covers_pointer(point));
        if covered {
            self.cancel_banner_press();
        }
        if self.chrome.is_some()
            && (!covered
                || (self.banner_wants_pointer_capture()
                    && !matches!(
                        event,
                        WindowEvent::MouseInput {
                            state: ElementState::Pressed,
                            button: winit::event::MouseButton::Left,
                            ..
                        }
                    )))
            && self.route_banner_pointer(event, ctx)
        {
            return true;
        }
        let is_absorbable_pointer = matches!(
            event,
            WindowEvent::CursorMoved { .. }
                | WindowEvent::MouseWheel { .. }
                | WindowEvent::MouseInput {
                    state: ElementState::Released,
                    ..
                }
        );
        if let WindowEvent::MouseWheel { delta, .. } = event
            && self.overlay_flags().ws_settings
            && self
                .last_cursor
                .is_some_and(|p| self.ws_settings_rect().contains(p))
            && self.workspace_settings_overlay.reviewing_agent().is_some()
        {
            let dy: f64 = match *delta {
                winit::event::MouseScrollDelta::LineDelta(_, y) => -f64::from(y) * 30.0,
                winit::event::MouseScrollDelta::PixelDelta(p) => -p.y,
            };
            if self.workspace_settings_overlay.scroll_agent_access(dy) {
                self.bump_chrome();
            }
            *ctx.handled = true;
            return true;
        }
        if self.route_ws_emoji_wheel(event) {
            *ctx.handled = true;
            return true;
        }
        if is_absorbable_pointer
            && let Some(p) = self.last_cursor
            && self.cursor_over_open_overlay(p)
        {
            // The palette owns hover before this guard isolates the app below.
            // Another covering overlay keeps its existing input priority.
            if self.palette_visible
                && self.search_card().contains(p)
                && !self.cursor_over_other_open_overlay(p)
            {
                let palette_child = ctx
                    .taffy
                    .children(ctx.node_id)
                    .ok()
                    .and_then(|children| children.get(1).copied());
                self.route_palette_event(event, ctx, palette_child);
            }
            *ctx.handled = true;
            return true;
        }
        false
    }

    /// `pump_permission_prompt` drains `pending_choice` next tick.
    /// Gated on the prompt's rect so its hit zone doesn't swallow
    /// clicks outside the bar.
    pub(super) fn route_permission_prompt_event(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        let Some(prompt) = self.permission_prompt.as_mut() else {
            return false;
        };
        let prompt_rect = prompt.get_rect();
        let forward = match event {
            WindowEvent::CursorMoved { .. } => true,
            WindowEvent::MouseInput { .. } => {
                self.last_cursor.is_some_and(|p| prompt_rect.contains(p))
            }
            _ => false,
        };
        if !forward {
            return false;
        }
        let mut tmp_taffy = TaffyTree::<()>::new();
        let Ok(tmp_node) = tmp_taffy.new_leaf(Style::default()) else {
            return false;
        };
        prompt.handle_event(
            event,
            &mut ui::widget::EventCtx {
                taffy: &tmp_taffy,
                ..ctx.reborrow(ctx.path, tmp_node)
            },
        );
        *ctx.handled
    }

    /// `DroppedFile` carries no coordinate, so route by `last_cursor`
    /// through the same hit-test click-focus uses. A drop over empty
    /// space is a no-op, not dumped into an arbitrary window.
    pub(super) fn route_dropped_file_event(
        &mut self,
        event: &WindowEvent,
        handled: &mut bool,
    ) -> bool {
        match event {
            WindowEvent::HoveredFile(_) => {
                self.set_content_drop_hover(self.last_cursor);
            }
            WindowEvent::HoveredFileCancelled => {
                self.set_content_drop_hover(None);
            }
            WindowEvent::DroppedFile(path) => {
                if let Some(p) = self.last_cursor {
                    let _ = self.open_dropped_path_at(path, p);
                }
                self.set_content_drop_hover(None);
            }
            _ => return false,
        }
        *handled = true;
        true
    }
}
