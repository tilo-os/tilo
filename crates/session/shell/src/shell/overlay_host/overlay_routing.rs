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
        if matches!(
            event,
            WindowEvent::Focused(false)
                | WindowEvent::MouseInput {
                    state: ElementState::Pressed,
                    button: winit::event::MouseButton::Left,
                    ..
                }
        ) {
            self.qs_navigation_release_pending = false;
        }
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
        if self.directory_sheet.is_open() && self.handle_directory_event(event, *ctx.modifiers) {
            *ctx.handled = true;
            return true;
        }

        if self.route_layer_band(Band::Modal, event, ctx) {
            return true;
        }

        if self.handle_projection_event(event, ctx) {
            *ctx.handled = true;
            return true;
        }

        false
    }

    /// Retains root navigation releases and slider drags before modal absorption.
    pub(super) fn route_qs_pointer_capture(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        handled: &mut bool,
    ) -> bool {
        if let WindowEvent::Touch(touch) = event
            && self.route_qs_slider_touch(touch, scale_factor)
        {
            *handled = true;
            return true;
        }
        if matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: winit::event::MouseButton::Left,
                ..
            }
        ) && std::mem::take(&mut self.qs_navigation_release_pending)
        {
            *handled = true;
            return true;
        }
        if self.now_playing_card.scrubbing() {
            match event {
                WindowEvent::CursorMoved { .. } => {
                    if let Some(point) = self.last_cursor {
                        self.drag_now_playing_seek(point);
                    }
                    *handled = true;
                    return true;
                }
                WindowEvent::MouseInput {
                    state: ElementState::Released,
                    button: winit::event::MouseButton::Left,
                    ..
                } => {
                    self.release_now_playing_seek();
                    *handled = true;
                    return true;
                }
                _ => {}
            }
        }
        if !self.qs_root.drag_active() {
            return false;
        }
        match event {
            WindowEvent::CursorMoved { .. } => {
                if let (Some(point), Some(chrome)) = (self.last_cursor, self.chrome.clone()) {
                    let panel = self.quick_settings_rect();
                    let mut st = chrome.borrow_mut();
                    if self
                        .qs_root
                        .update_drag(point, panel, &mut st.quick_settings)
                    {
                        st.bump();
                    }
                }
                *handled = true;
                true
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: winit::event::MouseButton::Left,
                ..
            } => {
                let finishing_layout_drag = self.qs_root.edit_mode();
                let commit = self
                    .chrome
                    .as_ref()
                    .and_then(|chrome| self.qs_root.finish_drag(&chrome.borrow().quick_settings));
                if let Some(commit) = commit {
                    self.commit_quick_settings_slider(commit);
                }
                if finishing_layout_drag && let Some(chrome) = self.chrome.as_ref() {
                    // `finish_drag` commits the shared reorder controller on
                    // release; publish that one durable layout change.
                    chrome.borrow_mut().bump();
                }
                *handled = true;
                true
            }
            _ => false,
        }
    }

    /// A finger on a Quick Settings slider works it as the pointer held down
    /// does: it lands as a press, the value follows it, and lifting commits.
    /// Whether the contact was a slider's.
    fn route_qs_slider_touch(&mut self, touch: &winit::event::Touch, scale_factor: f64) -> bool {
        use ui::widget::SliderTouchStep;
        let mut finger = self.qs_slider_touch;
        let step = finger.step(touch, scale_factor, |point| {
            self.qs_slider_face_at(point) || self.now_playing_seek_at(point)
        });
        self.qs_slider_touch = finger;
        match step {
            SliderTouchStep::Press(point) => {
                let p = self.untransform_quick_settings_pointer(point, Instant::now());
                if !self.handle_now_playing_click(p) {
                    self.handle_shell_quick_settings_root_click(p);
                }
                true
            }
            SliderTouchStep::Move(point) if self.now_playing_card.scrubbing() => {
                self.drag_now_playing_seek(point);
                true
            }
            SliderTouchStep::Release if self.now_playing_card.scrubbing() => {
                self.release_now_playing_seek();
                true
            }
            SliderTouchStep::Move(point) => {
                if let Some(chrome) = self.chrome.clone() {
                    let panel = self.quick_settings_rect();
                    let mut st = chrome.borrow_mut();
                    if self
                        .qs_root
                        .update_drag(point, panel, &mut st.quick_settings)
                    {
                        st.bump();
                    }
                }
                true
            }
            SliderTouchStep::Release => {
                let commit = self
                    .chrome
                    .as_ref()
                    .and_then(|chrome| self.qs_root.finish_drag(&chrome.borrow().quick_settings));
                if let Some(commit) = commit {
                    self.commit_quick_settings_slider(commit);
                }
                true
            }
            SliderTouchStep::Ignored => false,
        }
    }

    /// Whether a finger landing at `point` (logical, as drawn) is on a
    /// slider of the open Quick Settings root page.
    pub(super) fn qs_slider_face_at(&self, point: Point) -> bool {
        let Some(chrome) = self.chrome.as_ref() else {
            return false;
        };
        let Ok(st) = chrome.try_borrow() else {
            return false;
        };
        if !st.overlays.quick_settings
            || !matches!(
                st.quick_settings.current_level(),
                crate::shell::quick_settings::QsLevel::Root
            )
        {
            return false;
        }
        let p = self.untransform_quick_settings_pointer(point, Instant::now());
        self.quick_settings_rect().contains(p) && self.qs_root.slider_face_at(p, &st.quick_settings)
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
                if *repeat && self.portal_dialogs.review_pages().is_some() {
                    *ctx.handled = true;
                    return true;
                }
                let state = modifiers.state();
                let capture_chord = self.portal_dialogs.is_shortcut_editor()
                    && (state.control_key()
                        || state.alt_key()
                        || state.shift_key()
                        || state.super_key());
                if self.portal_dialogs.is_text_entry() {
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
        self.chrome.as_ref().is_some_and(|chrome| {
            let st = chrome.borrow();
            st.overlays.quick_settings
                && matches!(
                    st.quick_settings.current_level(),
                    crate::shell::quick_settings::QsLevel::Notifications
                )
                && self.quick_settings_rect().contains(point)
        })
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
            self.notification_stack.cancel_pointer_press();
        }
        if let Some(chrome) = self.chrome.clone()
            && (!covered
                || (self.notification_stack.wants_pointer_capture()
                    && !matches!(
                        event,
                        WindowEvent::MouseInput {
                            state: ElementState::Pressed,
                            button: winit::event::MouseButton::Left,
                            ..
                        }
                    )))
        {
            let mut banner_handled = false;
            let changed = self.notification_stack.handle_pointer_event(
                &ChromeNotifications::new(&chrome),
                event,
                ctx.scale_factor,
                &mut banner_handled,
            );
            if changed {
                chrome.borrow_mut().bump();
            }
            if banner_handled {
                *ctx.handled = true;
                return true;
            }
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
            && self.overlay_flags().shortcut_help
        {
            let dy: f64 = match *delta {
                winit::event::MouseScrollDelta::LineDelta(_, y) => -f64::from(y) * 30.0,
                winit::event::MouseScrollDelta::PixelDelta(p) => -p.y,
            };
            self.shortcut_guide_scroll =
                (self.shortcut_guide_scroll + dy).clamp(0.0, self.shortcut_guide_max_scroll);
            if let Some(chrome) = self.chrome.as_ref() {
                chrome.borrow_mut().bump();
            }
            *ctx.handled = true;
            return true;
        }
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
                && self.palette.get_rect().contains(p)
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
