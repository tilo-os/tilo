//! Where an [`App`] is shown: a window of its own or a region of a host's
//! surface. The app's input, focus, palette and presence handling run the
//! same way on either; only what a surface itself provides — its scale, its
//! size, a redraw, the pointer's shape — comes from the surface.

use super::*;

/// What an [`App`] needs from the surface it is shown on.
pub trait AppSurface {
    /// Physical pixels per logical pixel.
    fn scale_factor(&self) -> f64;
    /// The surface's size in physical pixels.
    fn inner_size(&self) -> winit::dpi::PhysicalSize<u32>;
    /// Ask for another frame.
    fn request_redraw(&self);
    /// Show `cursor` while the pointer is over the app.
    fn set_cursor(&self, cursor: CursorIcon);
    /// Whether the surface's host answers the desktop's tab chords (Ctrl+T,
    /// Ctrl+W, Ctrl+Shift+T) itself, so the app leaves them unhandled for it
    /// rather than asking the session's tab service.
    fn hosts_tab_chords(&self) -> bool {
        false
    }
}

impl AppSurface for winit::window::Window {
    fn scale_factor(&self) -> f64 {
        winit::window::Window::scale_factor(self)
    }
    fn inner_size(&self) -> winit::dpi::PhysicalSize<u32> {
        winit::window::Window::inner_size(self)
    }
    fn request_redraw(&self) {
        winit::window::Window::request_redraw(self);
    }
    fn set_cursor(&self, cursor: CursorIcon) {
        winit::window::Window::set_cursor(self, cursor);
    }
}

impl App {
    /// Restore the real focused native configuration after modal/page
    /// replacement, without replaying any key press or typed input.
    pub(super) fn restore_physical_configuration(&mut self) {
        if self.close_admission.dialog.is_none()
            && self
                .history
                .as_ref()
                .is_none_or(|history| history.live_interactive())
            && self.history_layout.is_none_or(|layout| layout.app_visible)
            && let Some(layout) = self.input.keyboard_layout.clone()
        {
            self.root_widget
                .observe_physical_input(crate::widget::PhysicalInput::Layout(Some(layout)));
        }
    }

    /// Handle `event` from the surface the app is shown on. Pointer
    /// positions are in the surface's physical pixels, relative to the
    /// app's own origin. `synthesized_touch` marks pointer events the host
    /// made from a touch, which keep touch modality. Returns whether
    /// anything handled the event.
    #[allow(
        clippy::too_many_lines,
        reason = "input routing shares mutable state and must retain fixed precedence"
    )]
    pub fn surface_input(
        &mut self,
        window: &dyn AppSurface,
        event: WindowEvent,
        synthesized_touch: bool,
    ) -> bool {
        let _entry = self.enter_wake_scope();
        if matches!(
            &event,
            WindowEvent::KeyboardInput {
                event: winit::event::KeyEvent {
                    state: ElementState::Pressed,
                    ..
                },
                ..
            } | WindowEvent::MouseInput {
                state: ElementState::Pressed,
                ..
            } | WindowEvent::MouseWheel { .. }
                | WindowEvent::Touch(winit::event::Touch {
                    phase: winit::event::TouchPhase::Started,
                    ..
                })
                | WindowEvent::Ime(winit::event::Ime::Commit(_))
        ) {
            self.take_over_owner_view();
        }
        match &event {
            WindowEvent::KeyboardLayoutChanged(layout) => {
                self.input.keyboard_layout.clone_from(layout)
            }
            WindowEvent::Focused(false) => self.input.keyboard_layout = None,
            _ => {}
        }
        self.restore_physical_configuration();
        if self.close_admission.dialog.is_some()
            || self
                .history
                .as_ref()
                .is_some_and(|history| !history.live_interactive())
            || self
                .history_layout
                .is_some_and(|layout| !layout.app_visible)
        {
            self.root_widget
                .observe_physical_input(crate::widget::PhysicalInput::Focused(false));
        } else if let Some(input) = crate::widget::PhysicalInput::from_window_event(&event) {
            self.root_widget.observe_physical_input(input);
        }
        if self.close_handle_event(&event, window.scale_factor()) {
            self.root_widget
                .observe_physical_input(crate::widget::PhysicalInput::Focused(false));
            window.request_redraw();
            return true;
        }
        let app = self;
        let history_input = app.history_controls_event(&event, window.scale_factor());
        let _history_frame = crate::history_mode::frame::FrameworkInputFrame::new(
            &app.framework_input,
            history_input,
        );
        let detected_interaction = if synthesized_touch {
            None
        } else {
            match &event {
                WindowEvent::Touch(_) => Some(app.input.interaction.with_touch_modality()),
                WindowEvent::CursorMoved { .. }
                | WindowEvent::MouseInput { .. }
                | WindowEvent::MouseWheel { .. } => {
                    Some(app.input.interaction.with_pointer_modality())
                }
                WindowEvent::KeyboardInput {
                    event:
                        winit::event::KeyEvent {
                            state: ElementState::Pressed,
                            ..
                        },
                    ..
                } => Some(app.input.interaction.with_keyboard_modality()),
                _ => None,
            }
        };
        if let Some(environment) = detected_interaction {
            app.set_interaction_environment(environment);
        }
        if history_input {
            app.dispatch_framework_history_event(&event, window.scale_factor());
            window.request_redraw();
            return true;
        }
        let __prof_input = app.profiler.as_ref().map(|_| crate::time::now());
        // RUST_LOG=tilo_ui::app=debug: pointer-move spam is intentionally
        // not throttled here — when the bug is "no clicks work", you
        // want to see whether moves arrive at all.
        match &event {
            WindowEvent::MouseInput { state, button, .. } => {
                tracing::debug!(?state, ?button, "App::window_event MouseInput");
            }
            WindowEvent::CursorMoved { position, .. } => {
                tracing::trace!(
                    x = position.x,
                    y = position.y,
                    "App::window_event CursorMoved"
                );
            }
            WindowEvent::KeyboardInput { event: ke, .. } => {
                tracing::debug!(key = ?ke.logical_key, state = ?ke.state, "App::window_event KeyboardInput");
            }
            _ => {}
        }
        // `CursorMoved` is measured in this window's own content,
        // never the desktop — see the `pointer_space` field.
        if app.presence.broadcast_scope.is_some() {
            match &event {
                WindowEvent::CursorMoved { position, .. } => {
                    let now = crate::time::now();
                    if pointer_broadcast_allowed(
                        app.presence.last_pointer_broadcast,
                        now,
                        Duration::from_millis(POINTER_BROADCAST_MIN_GAP_MS),
                    ) {
                        app.presence.last_pointer_broadcast = Some(now);
                        let where_it_is = app.local_pointer_key();
                        #[allow(
                            clippy::cast_possible_truncation,
                            reason = "presence coordinates are intentionally narrowed to f32 on the wire; identical to today's output"
                        )]
                        app.broadcast_presence(
                            where_it_is,
                            PresencePayload::Pointer {
                                x: position.x as f32,
                                y: position.y as f32,
                            },
                        );
                    }
                }
                WindowEvent::CursorLeft { .. } => {
                    let where_it_was = app.local_pointer_key();
                    app.broadcast_presence(where_it_was, PresencePayload::Left);
                    app.presence.last_pointer_broadcast = None;
                }
                _ => {}
            }
        }

        let mut handled = false;
        let mut history_handled = matches!(event, WindowEvent::KeyboardInput { .. })
            && app
                .focused_node
                .is_some_and(|node| app.is_history_node(node));
        let mut focus_target = None;
        // A synthesized hover-clear may be handled by a control, but it is
        // presentation state and must not create an empty undo boundary.
        let mut is_undo_redo =
            synthesized_touch && matches!(event, WindowEvent::CursorMoved { .. });
        let mut floating_pointer_needs_redraw = false;

        // The palette owns the keyboard while open; Ctrl+K itself is
        // handled below, so let it through.
        if app.actions.action_palette.is_some() {
            let is_ctrl_k = matches!(
                &event,
                WindowEvent::KeyboardInput {
                    event: winit::event::KeyEvent {
                        logical_key: winit::keyboard::Key::Character(c),
                        state: winit::event::ElementState::Pressed,
                        ..
                    },
                    ..
                } if app
                    .input
                    .platform
                    .command_modifier_down(app.input.modifiers.state())
                    && crate::platform::HostShortcut::resolve(
                        app.input.platform,
                        c,
                        app.input.modifiers.state().shift_key(),
                    ) == Some(crate::platform::HostShortcut::Search)
            );
            if !is_ctrl_k
                && matches!(
                    event,
                    WindowEvent::KeyboardInput { .. }
                        | WindowEvent::Ime(_)
                        | WindowEvent::CursorMoved { .. }
                        | WindowEvent::CursorLeft { .. }
                        | WindowEvent::MouseInput { .. }
                        | WindowEvent::MouseWheel { .. }
                )
                && app.action_palette_handle_event(&event, window.scale_factor())
            {
                handled = true;
            }
        }

        // Runs FIRST so it wins over Tab/arrow handling.
        if app.handle_screen_reader_key(&event) {
            handled = true;
            is_undo_redo = true; // suppress an undo commit for a read cmd
        }

        let state_pre = if matches!(
            event,
            WindowEvent::KeyboardInput { .. }
                | WindowEvent::MouseInput {
                    state: winit::event::ElementState::Pressed,
                    ..
                }
        ) {
            app.focused_state_snapshot()
        } else {
            None
        };

        let review_pre = if matches!(event, WindowEvent::KeyboardInput { .. }) {
            app.text_review_snapshot()
        } else {
            None
        };

        // Governs pointer click-through only; keyboard focus
        // containment is the separate `traps_focus()` concern (see
        // `keyboard_nav_allowed`).
        let modal_rect = app.root_widget.modal_capture();

        if !handled
            && let WindowEvent::KeyboardInput {
                event:
                    winit::event::KeyEvent {
                        logical_key: winit::keyboard::Key::Named(winit::keyboard::NamedKey::F12),
                        state: winit::event::ElementState::Pressed,
                        ..
                    },
                ..
            } = event
        {
            app.debug_mode = !app.debug_mode;
            handled = true;
        }

        if !handled
            && app
                .input
                .platform
                .command_modifier_down(app.input.modifiers.state())
            && let WindowEvent::KeyboardInput {
                event:
                    winit::event::KeyEvent {
                        logical_key: winit::keyboard::Key::Character(ref c),
                        state: winit::event::ElementState::Pressed,
                        ..
                    },
                ..
            } = event
        {
            let shift = app.input.modifiers.state().shift_key();
            match crate::platform::HostShortcut::resolve(app.input.platform, c, shift) {
                Some(crate::platform::HostShortcut::Undo) => {
                    app.perform_undo_request(crate::undo::UndoRequest::Undo);
                    is_undo_redo = true;
                    handled = true;
                }
                Some(crate::platform::HostShortcut::Redo) => {
                    app.perform_undo_request(crate::undo::UndoRequest::Redo);
                    is_undo_redo = true;
                    handled = true;
                }
                // A second press closes the palette.
                Some(crate::platform::HostShortcut::Search) => {
                    if app.actions.action_palette.is_some() {
                        app.close_action_palette();
                    } else {
                        app.open_action_palette();
                    }
                    handled = true;
                }
                Some(crate::platform::HostShortcut::History) => {
                    handled = app.invoke_host_shortcut(crate::platform::HostShortcut::History);
                }
                Some(crate::platform::HostShortcut::Sidebar) => {
                    handled = app.invoke_host_shortcut(crate::platform::HostShortcut::Sidebar);
                }
                None => {}
            }
            if handled {
                window.request_redraw();
            }
        }

        if !handled && app.handle_tab_navigation(&event, modal_rect) {
            handled = true;
        }

        if !handled
            && let WindowEvent::KeyboardInput {
                event:
                    winit::event::KeyEvent {
                        logical_key: winit::keyboard::Key::Named(logical_key),
                        state: winit::event::ElementState::Pressed,
                        ..
                    },
                ..
            } = event
        {
            let dir = match logical_key {
                winit::keyboard::NamedKey::ArrowUp => Some(Direction::Up),
                winit::keyboard::NamedKey::ArrowDown => Some(Direction::Down),
                winit::keyboard::NamedKey::ArrowLeft => Some(Direction::Left),
                winit::keyboard::NamedKey::ArrowRight => Some(Direction::Right),
                _ => None,
            };
            if let (Some(d), true, true) = (
                dir,
                app.input.modifiers.state().alt_key(),
                app.keyboard_nav_allowed(modal_rect),
            ) {
                app.navigate_focus(d);
                handled = true;
            }
        }

        // Two-finger swipe, edge pull or mouse side button; delivered via
        // `on_nav_gesture`.
        if !handled && !synthesized_touch {
            use crate::gesture::GesturePhase;
            let map_phase = |p: winit::event::TouchPhase| match p {
                winit::event::TouchPhase::Started => GesturePhase::Begin,
                winit::event::TouchPhase::Moved => GesturePhase::Update,
                winit::event::TouchPhase::Ended => GesturePhase::End,
                winit::event::TouchPhase::Cancelled => GesturePhase::Cancel,
            };
            let nav = match &event {
                WindowEvent::MouseWheel { delta, phase, .. } => {
                    let (dx, dy, precise) = match delta {
                        winit::event::MouseScrollDelta::PixelDelta(p) => (p.x, p.y, true),
                        winit::event::MouseScrollDelta::LineDelta(x, y) => {
                            (f64::from(*x), f64::from(*y), false)
                        }
                    };
                    app.input
                        .nav_gestures
                        .on_scroll(dx, dy, map_phase(*phase), precise)
                }
                WindowEvent::PanGesture { delta, phase, .. } => app.input.nav_gestures.on_scroll(
                    f64::from(delta.x),
                    f64::from(delta.y),
                    map_phase(*phase),
                    true,
                ),
                WindowEvent::MouseInput { .. } => crate::interaction::mouse_nav_direction(&event),
                WindowEvent::Touch(t) => {
                    let width = f64::from(window.inner_size().width);
                    app.input.nav_gestures.on_touch(
                        t.id,
                        t.location.x,
                        t.location.y,
                        map_phase(t.phase),
                        width,
                    )
                }
                _ => None,
            };
            if let Some(dir) = nav {
                let consumed = app
                    .root_widget
                    .on_nav_gesture(dir, &app.state.doc, &app.state.root);
                if consumed {
                    app.needs_sync = true;
                    window.request_redraw();
                    handled = true;
                }
            }
        }

        if !handled && app.layout.root_node.is_some() {
            app.refresh_control_action_availability();
            let scale_factor = window.scale_factor();
            let presentation_was_floating =
                !app.root_widget.floating_surface_presentations().is_empty();
            let floating_pointer_event = app.primary_floating_pointer_event(&event, scale_factor);
            let used_desktop_pointer_space = floating_pointer_event.is_some();
            let event_for_root = floating_pointer_event.as_ref().unwrap_or(&event);
            let was_mouse_click = matches!(
                event,
                WindowEvent::MouseInput {
                    state: winit::event::ElementState::Pressed,
                    ..
                }
            );
            history_handled |= app.presentation_handle_event(
                event_for_root,
                scale_factor,
                &mut handled,
                &mut focus_target,
            );
            // Even a document-free interaction may switch which resources a
            // widget presents. Invalidate once at the mutable tree boundary;
            // all later consumers in this event/frame share the rebuilt cache.
            app.invalidate_widget_resource_cache();
            let presentation_is_floating =
                !app.root_widget.floating_surface_presentations().is_empty();
            floating_pointer_needs_redraw = matches!(event, WindowEvent::CursorMoved { .. })
                && (used_desktop_pointer_space
                    || presentation_was_floating != presentation_is_floating);
            if was_mouse_click {
                tracing::debug!(
                    handled,
                    needs_redraw = app.root_widget.needs_redraw(),
                    is_layout_dirty = app.root_widget.is_layout_dirty(),
                    "root_widget.handle_event(MouseInput Pressed) returned"
                );
            }
            // A press the modal didn't consume (the dimmed backdrop)
            // is still marked handled so it can't reach widgets
            // behind the scrim.
            if modal_rect.is_some()
                && matches!(
                    event,
                    WindowEvent::MouseInput {
                        state: winit::event::ElementState::Pressed,
                        ..
                    } | WindowEvent::MouseInput {
                        state: winit::event::ElementState::Released,
                        ..
                    }
                )
            {
                handled = true;
            }
        }

        // Alt+Arrow remains host directional navigation when the focused
        // widget leaves it unhandled. Running this after widget dispatch lets
        // editors consume platform word/selection shortcuts first, while
        // keeping Alt navigation ahead of the plain range-control fallback.
        app.handle_unconsumed_directional_key(&event, modal_rect, true, &mut handled);

        // If no widget consumed Enter/Space (text inputs do), activate
        // the focused control the same way a mouse click would.
        app.handle_unconsumed_activation_key(&event, &mut handled);

        if !handled && app.handle_unconsumed_search_key(&event) {
            handled = true;
        }

        // Ctrl+T / Ctrl+W / Ctrl+Shift+T nobody consumed are the desktop's
        // tab chords.
        if !window.hosts_tab_chords() {
            app.handle_unconsumed_tab_chord(&event, &mut handled);
        }

        // A focused range control consumes Left/Right; Up/Down and Tab
        // still move focus. Modifiers scale the step:
        //   plain  → one step       Shift → four steps (coarse)
        //   Ctrl   → jump to min/max (Left = min, Right = max)
        // Other widgets ignore the action, falling through to
        // directional navigation below.
        if !handled
            && let WindowEvent::KeyboardInput {
                event:
                    winit::event::KeyEvent {
                        logical_key: winit::keyboard::Key::Named(named),
                        state: winit::event::ElementState::Pressed,
                        ..
                    },
                ..
            } = event
        {
            use crate::a11y::Action;
            let ctrl = app
                .input
                .platform
                .command_modifier_down(app.input.modifiers.state());
            let shift = app.input.modifiers.state().shift_key();
            // (action to repeat, how many times)
            let (action, times) = match named {
                winit::keyboard::NamedKey::ArrowLeft if ctrl => {
                    (Some(Action::SetValue("0".into())), 1)
                }
                winit::keyboard::NamedKey::ArrowRight if ctrl => {
                    (Some(Action::SetValue("100".into())), 1)
                }
                winit::keyboard::NamedKey::ArrowLeft => {
                    (Some(Action::Decrement), if shift { 4 } else { 1 })
                }
                winit::keyboard::NamedKey::ArrowRight => {
                    (Some(Action::Increment), if shift { 4 } else { 1 })
                }
                _ => (None, 0),
            };
            if let (Some(act), Some(fid)) = (action, app.focused_node) {
                let mut any = false;
                for _ in 0..times {
                    any |= app
                        .dispatch_node(fid, |widget, doc, root| {
                            widget.perform_action(&act, &mut crate::widget::Edits::lend(doc), root)
                        })
                        .unwrap_or(false);
                }
                if any {
                    handled = true;
                    app.needs_sync = true;
                    app.layout.layout_dirty = true;
                }
            }
        }

        app.handle_unconsumed_directional_key(&event, modal_rect, false, &mut handled);

        if app.dispatch_control_invocations() != 0 {
            handled = true;
            app.needs_sync = true;
            app.layout.layout_dirty = true;
            window.request_redraw();
        }

        if let Some(address) = app.root_widget.take_action_palette_request()
            && app.open_action_palette_at(&address)
        {
            app.layout.layout_dirty = true;
            window.request_redraw();
        }

        if app.root_widget.take_search_request() && app.actions.action_palette.is_none() {
            app.open_action_palette();
            app.layout.layout_dirty = true;
            window.request_redraw();
        }

        app.broadcast_focus_presence();
        app.broadcast_location_presence();
        app.broadcast_view_presence();
        app.publish_action_surface();
        app.publish_continuation_metadata();
        app.publish_playback_handoff();

        if let Some(id) = focus_target {
            app.set_focused_node(Some(id));
            handled = true;
        }

        app.announce_state_change(state_pre);
        app.announce_text_review(review_pre);

        if handled {
            app.finalize_handled_native_event(&event, is_undo_redo, history_handled);
            app.needs_sync = true;
            // Gating every handled event on `needs_redraw()` is
            // wrong: container widgets can mutate their own
            // active-index state without setting a descendant leaf's
            // `dirty` flag, so recursive `needs_redraw` misses it.
            // Only pointer MOVES (the hot 60+ Hz stream) skip a
            // redraw when nothing's dirty.
            let is_pointer_move = matches!(&event, WindowEvent::CursorMoved { .. });
            if !is_pointer_move || app.root_widget.is_layout_dirty() {
                app.layout.layout_dirty = true;
            }
            if !is_pointer_move
                || floating_pointer_needs_redraw
                || app.root_widget.needs_redraw()
                || app.history_needs_redraw()
            {
                window.request_redraw();
            }
        }

        // Resolved on every `CursorMoved`, not just handled ones, so
        // the shape stays correct crossing into a widget that
        // doesn't flip its own hover.
        if let WindowEvent::CursorMoved { position, .. } = &event {
            let scale = window.scale_factor();
            let logical = Point::new(position.x / scale, position.y / scale);
            let desired = app.resolve_cursor(logical).unwrap_or(CursorIcon::Default);
            if app.input.current_cursor != desired {
                app.input.current_cursor = desired;
                window.set_cursor(desired);
            }
            // `on_pointer` keeps the dwell running over the same
            // region and restarts it on a new one.
            let target = app.tooltip_target_at(logical);
            if app
                .input
                .tooltip_host
                .on_pointer(target, crate::time::now())
            {
                window.request_redraw();
            }
        }

        if matches!(
            &event,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                ..
            } | WindowEvent::MouseWheel { .. }
                | WindowEvent::KeyboardInput { .. }
                | WindowEvent::Touch(_)
                | WindowEvent::CursorLeft { .. }
        ) && app.input.tooltip_host.dismiss()
        {
            window.request_redraw();
        }

        if let (Some(start), Some(p)) = (__prof_input, app.profiler.as_mut()) {
            p.record(crate::profiling::Pass::HandleEvent, start.elapsed());
        }
        handled
    }
    /// Whether the host the app is shown in continues its focus order past
    /// the app, so Tab at either end of the app's order leaves the app.
    pub fn set_tab_order_continues_in_host(&mut self, continues: bool) {
        self.input.tab_order_continues_in_host = continues;
    }

    /// Focus the first control of the app's order, or the last with `last`:
    /// where keyboard focus lands when it enters the app from its host.
    /// Returns whether the app has a control to focus.
    pub fn focus_order_edge(&mut self, last: bool) -> bool {
        let edge = if last {
            self.focusable_order.last()
        } else {
            self.focusable_order.first()
        };
        let Some(&id) = edge else {
            return false;
        };
        self.set_focused_node(Some(id));
        true
    }

    /// The control with keyboard focus, to restore when focus comes back.
    pub fn focused_control(&self) -> Option<NodeId> {
        self.focused_node
    }

    /// Give keyboard focus back to `control` if it is still in the app's
    /// focus order, or take it from every control with `None`.
    pub fn restore_focused_control(&mut self, control: Option<NodeId>) -> bool {
        match control {
            Some(id) if self.focusable_order.contains(&id) => {
                self.set_focused_node(Some(id));
                true
            }
            Some(_) => false,
            None => {
                self.set_focused_node(None);
                true
            }
        }
    }

    /// Whether the last frame left a sync or a layout to settle.
    pub fn unsettled(&self) -> bool {
        self.needs_sync || self.layout.layout_dirty || self.layout.structure_dirty
    }

    /// The modifier keys held, as the surface reports them.
    pub fn set_surface_modifiers(&mut self, modifiers: winit::event::Modifiers) {
        self.input.modifiers = modifiers;
    }

    /// Run one frame of the app on `surface` and paint it into `scene`, in
    /// the surface's logical pixels from the app's own origin: its model
    /// pumped (collaboration, invocations, publication), synchronized, laid
    /// out and drawn, as a window of its own runs it. Returns whether the app
    /// wants another frame.
    pub fn surface_frame(&mut self, surface: &dyn AppSurface, scene: &mut Scene) -> bool {
        let _entry = self.enter_wake_scope();
        let replaced = self.swap_requested_component();
        // The model's own work, as a hosted worker's pump runs it:
        // collaboration (inbound edits, directory requests, publication),
        // invocations, idle model work and the record.
        self.pump_hosted_model();
        if self.layout.root_node.is_none() {
            self.init_layout();
        }
        let size = surface.inner_size();
        #[allow(
            clippy::cast_possible_truncation,
            reason = "scale factor is intentionally narrowed to f32 for the render pipeline"
        )]
        let scale_factor = surface.scale_factor() as f32;
        self.layout.last_frame_size = Some(size);
        self.layout.structure_dirty |= self.root_widget.take_structure_dirty() || replaced;
        let structure_rebuilt = self.layout.structure_dirty;
        if structure_rebuilt {
            self.rebuild_structure();
        }
        let Some(root_node) = self.layout.root_node else {
            return true;
        };
        if run_layout_pass(
            self,
            root_node,
            size.width,
            size.height,
            scale_factor,
            structure_rebuilt,
            None,
        )
        .is_none()
        {
            return true;
        }
        let search_pending = self
            .actions
            .action_palette
            .as_mut()
            .is_some_and(|palette| palette.advance_search_page().pending);
        draw_frame_content(self, scene, size.width, size.height, scale_factor, None);
        complete_native_frame_content(self, |_| {});
        search_pending
            || self.needs_sync
            || self.layout.layout_dirty
            || self.history_needs_redraw()
            || self
                .root_widget
                .next_wake_deadline()
                .is_some_and(|deadline| deadline <= crate::time::now())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widget::TaffyTreeExt;
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    thread_local! {
        static CLICKS: RefCell<Option<Rc<RefCell<Vec<Point>>>>> = const { RefCell::new(None) };
        static PHYSICAL: RefCell<Vec<crate::widget::PhysicalInput>> = const { RefCell::new(Vec::new()) };

    }

    include!("native_publication_scroll_tests.rs");

    /// Records where it is clicked, in its own logical coordinates.
    struct ClickProbe {
        rect: KurboRect,
        pointer: Point,
        clicks: Rc<RefCell<Vec<Point>>>,
    }

    impl Widget for ClickProbe {
        fn observe_physical_input(&mut self, input: crate::widget::PhysicalInput) {
            PHYSICAL.with_borrow_mut(|events| events.push(input));
        }
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _theme: &Theme,
            nodes: &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            let style = taffy::Style {
                size: taffy::Size {
                    width: taffy::Dimension::percent(1.0),
                    height: taffy::Dimension::percent(1.0),
                },
                ..taffy::Style::default()
            };
            taffy.widget_leaf(style, nodes, self)
        }
        fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
            let layout = taffy.layout(node).unwrap();
            self.rect = crate::widget::placed(layout, origin);
        }
        fn draw(
            &mut self,
            scene: &mut Scene,
            transform: Affine,
            _text: &mut TextRenderer,
            theme: &Theme,
            _presence: &PresenceCtx,
        ) {
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                theme.tokens.colors.surfaces.base,
                None,
                &self.rect,
            );
        }
        fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
            match event {
                WindowEvent::CursorMoved { position, .. } => {
                    self.pointer =
                        Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                }
                WindowEvent::MouseInput {
                    state: ElementState::Pressed,
                    ..
                } if self.rect.contains(self.pointer) => {
                    self.clicks.borrow_mut().push(self.pointer);
                    *ctx.handled = true;
                }
                _ => {}
            }
        }
        fn name(&self) -> &'static str {
            "Clicks"
        }
        fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
            let mut node = crate::a11y::AccessNode::new(crate::a11y::Role::Button);
            node.name = Some("Press".into());
            node.state.focusable = true;
            Some(node)
        }
        fn is_focusable(&self) -> bool {
            true
        }
        fn get_rect(&self) -> KurboRect {
            self.rect
        }
    }

    impl crate::widget::AppWidget for ClickProbe {
        fn app_kind(&self) -> crate::widget::AppKind {
            crate::widget::AppKind::Editor
        }
        fn app_title(&self) -> &'static str {
            "Clicks"
        }
        fn app_icon(&self) -> crate::widget::IconDescriptor {
            crate::widget::IconDescriptor::Glyph("C")
        }
        fn permission_hint(&self) -> Option<crate::widget::AppKind> {
            None
        }
    }

    fn clicks_factory(
        mut context: crate::application::HostContext,
        _: crate::application::LaunchTarget,
        _: crate::application::RestoreInput,
    ) -> Result<crate::application::AppComponent, crate::launch::LaunchError> {
        context
            .initialize_new_record(|_| Ok(()))
            .map_err(crate::launch::LaunchError::Record)?;
        let clicks = Rc::new(RefCell::new(Vec::new()));
        CLICKS.with_borrow_mut(|slot| *slot = Some(Rc::clone(&clicks)));
        Ok(crate::application::AppComponent::new(
            context,
            Box::new(ClickProbe {
                rect: KurboRect::ZERO,
                pointer: Point::ZERO,
                clicks,
            }),
            tilo_actions::ActionRegistry::new(),
        ))
    }

    /// A region of a host's scene, in logical pixels.
    struct Region {
        size: (u32, u32),
        redraw: Cell<bool>,
    }

    impl AppSurface for Region {
        fn scale_factor(&self) -> f64 {
            1.0
        }
        fn inner_size(&self) -> winit::dpi::PhysicalSize<u32> {
            winit::dpi::PhysicalSize::new(self.size.0, self.size.1)
        }
        fn request_redraw(&self) {
            self.redraw.set(true);
        }
        fn set_cursor(&self, _cursor: CursorIcon) {}
    }

    /// An app shown in a region of a host's scene lays out at the region's
    /// size, paints there, and takes the region's pointer input in its own
    /// coordinates, as it would in a window of that size.
    #[test]
    fn an_app_on_a_host_surface_lays_out_paints_and_takes_input_there() {
        let host = crate::application_test_support::ProbeHost::new();
        let definition = crate::application_test_support::definition(clicks_factory);
        let target = crate::application::LaunchTarget::New;
        let context = host
            .prepare_with_permissions(
                &definition,
                71,
                &target,
                crate::application_test_support::runner_permissions(),
            )
            .unwrap();
        let component = definition
            .instantiate(context, target, crate::application::RestoreInput::None)
            .unwrap();
        let mut app =
            App::from_component(component, crate::application_test_support::runner_inputs())
                .unwrap();
        let clicks = CLICKS.with_borrow(|slot| Rc::clone(slot.as_ref().unwrap()));
        let region = Region {
            size: (320, 200),
            redraw: Cell::new(false),
        };

        let mut scene = Scene::new();
        let _ = app.surface_frame(&region, &mut scene);
        assert!(
            !scene.encoding().is_empty(),
            "the app paints into the host's scene"
        );
        assert_eq!(
            app.root_widget.get_rect().size(),
            vello::kurbo::Size::new(320.0, 200.0),
            "the app lays out at the region's size"
        );

        let device = winit::event::DeviceId::dummy();
        assert!(!app.surface_input(
            &region,
            WindowEvent::CursorMoved {
                device_id: device,
                position: (40.0, 30.0).into(),
            },
            false,
        ));
        assert!(app.surface_input(
            &region,
            WindowEvent::MouseInput {
                device_id: device,
                state: ElementState::Pressed,
                button: winit::event::MouseButton::Left,
            },
            false,
        ));
        assert_eq!(*clicks.borrow(), vec![Point::new(40.0, 30.0)]);
    }

    #[test]
    fn physical_feedback_precedes_framework_shortcuts_without_consuming_them() {
        use winit::keyboard::{Key, KeyCode, KeyLocation, PhysicalKey};
        let (mut app, _) = clicks_app();
        let region = Region {
            size: (320, 200),
            redraw: Cell::new(false),
        };
        app.surface_frame(&region, &mut Scene::new());
        app.input.modifiers =
            winit::event::Modifiers::from(winit::keyboard::ModifiersState::CONTROL);
        PHYSICAL.with_borrow_mut(Vec::clear);
        let key = PhysicalKey::Code(KeyCode::KeyZ);
        let event = WindowEvent::KeyboardInput {
            device_id: winit::event::DeviceId::dummy(),
            event: winit::event::KeyEvent::synthetic(
                winit::event::RawKeyEvent {
                    physical_key: key,
                    state: ElementState::Pressed,
                },
                Key::Character("z".into()),
                None,
                KeyLocation::Standard,
                false,
                None,
                Key::Character("z".into()),
            ),
            is_synthetic: false,
        };
        assert!(
            app.surface_input(&region, event.clone(), false),
            "framework Undo still handles the chord"
        );
        PHYSICAL.with_borrow(|events| {
            assert_eq!(events.len(), 1);
            assert!(matches!(events[0], crate::widget::PhysicalInput::Key {
                key: observed, state: ElementState::Pressed
            } if observed == key));
        });
        let mut synthetic = event;
        if let WindowEvent::KeyboardInput { is_synthetic, .. } = &mut synthetic {
            *is_synthetic = true;
        }
        assert!(crate::widget::PhysicalInput::from_window_event(&synthetic).is_none());
        app.surface_input(&region, synthetic, false);
        PHYSICAL.with_borrow(|events| assert_eq!(events.len(), 1));
        app.surface_input(&region, WindowEvent::Focused(false), false);
        PHYSICAL.with_borrow(|events| {
            assert!(matches!(
                events.last(),
                Some(crate::widget::PhysicalInput::Focused(false))
            ))
        });
    }

    #[test]
    fn physical_feedback_clears_when_framework_modal_takes_input() {
        let (mut app, _) = clicks_app();
        let region = Region {
            size: (320, 200),
            redraw: Cell::new(false),
        };
        app.surface_frame(&region, &mut Scene::new());
        app.close_admission.show(
            "close-title",
            "close-unsaved",
            &[crate::close_admission::Choice::KeepEditing],
            app.input.platform,
        );
        app.layout.structure_dirty = true;
        app.layout.layout_dirty = true;
        app.surface_frame(&region, &mut Scene::new());
        PHYSICAL.with_borrow_mut(Vec::clear);
        app.surface_input(
            &region,
            WindowEvent::KeyboardInput {
                device_id: winit::event::DeviceId::dummy(),
                event: winit::event::KeyEvent::synthetic(
                    winit::event::RawKeyEvent {
                        physical_key: winit::keyboard::PhysicalKey::Code(
                            winit::keyboard::KeyCode::KeyY,
                        ),
                        state: ElementState::Pressed,
                    },
                    winit::keyboard::Key::Character("private-modal-input".into()),
                    None,
                    winit::keyboard::KeyLocation::Standard,
                    false,
                    None,
                    winit::keyboard::Key::Character("private-modal-input".into()),
                ),
                is_synthetic: false,
            },
            false,
        );
        PHYSICAL.with_borrow(|events| {
            assert!(
                !events.is_empty(),
                "modal priority clears previous held positions"
            );
            assert!(
                events
                    .iter()
                    .all(|event| matches!(event, crate::widget::PhysicalInput::Focused(false))),
                "modal keystrokes must not reach foreground observers"
            );
        });
    }

    #[test]
    fn native_configuration_resumes_after_modal_without_replaying_private_input() {
        let (mut app, _) = clicks_app();
        let region = Region {
            size: (320, 200),
            redraw: Cell::new(false),
        };
        app.surface_frame(&region, &mut Scene::new());
        let snapshot = |text: &str| {
            std::sync::Arc::new(winit::keyboard::KeyboardLayout {
                depressed: 0,
                latched: 0,
                locked: 0,
                group: 1,
                keys: vec![winit::keyboard::KeyboardLayoutKey {
                    physical: winit::keyboard::PhysicalKey::Code(winit::keyboard::KeyCode::KeyY),
                    symbols: vec![winit::keyboard::KeyboardLayoutSymbol {
                        name: text.into(),
                        text: text.into(),
                    }],
                }],
            })
        };
        app.surface_input(
            &region,
            WindowEvent::KeyboardLayoutChanged(Some(snapshot("z"))),
            false,
        );
        app.close_admission.show(
            "close-title",
            "close-unsaved",
            &[crate::close_admission::Choice::KeepEditing],
            app.input.platform,
        );
        PHYSICAL.with_borrow_mut(Vec::clear);
        let active = snapshot("Z");
        let heads = app.state.doc.get_heads();
        app.surface_input(
            &region,
            WindowEvent::KeyboardLayoutChanged(Some(active.clone())),
            false,
        );
        PHYSICAL.with_borrow(|events| {
            assert!(
                events
                    .iter()
                    .all(|event| matches!(event, crate::widget::PhysicalInput::Focused(false)))
            )
        });
        app.close_admission.dialog = None;
        app.surface_frame(&region, &mut Scene::new());
        PHYSICAL.with_borrow(|events| assert!(events.iter().any(|event|
            matches!(event, crate::widget::PhysicalInput::Layout(Some(layout)) if layout == &active))));
        assert_eq!(
            app.state.doc.get_heads(),
            heads,
            "configuration never edits the document"
        );
        app.surface_input(&region, WindowEvent::Focused(false), false);
        PHYSICAL.with_borrow_mut(Vec::clear);
        app.surface_frame(&region, &mut Scene::new());
        PHYSICAL.with_borrow(|events| {
            assert!(
                events.is_empty(),
                "actual window blur revokes native metadata"
            )
        });
    }

    fn clicks_app() -> (App, Rc<RefCell<Vec<Point>>>) {
        let host = crate::application_test_support::ProbeHost::new();
        let definition = crate::application_test_support::definition(clicks_factory);
        let target = crate::application::LaunchTarget::New;
        let context = host
            .prepare_with_permissions(
                &definition,
                72,
                &target,
                crate::application_test_support::runner_permissions(),
            )
            .unwrap();
        let component = definition
            .instantiate(context, target, crate::application::RestoreInput::None)
            .unwrap();
        let app = App::from_component(component, crate::application_test_support::runner_inputs())
            .unwrap();
        let clicks = CLICKS.with_borrow(|slot| Rc::clone(slot.as_ref().unwrap()));
        (app, clicks)
    }

    /// An app embedded in a host's tree is laid out where the host places
    /// it, takes the host's pointer input in its own coordinates whatever
    /// the host's scale, and leaves the desktop's tab chords to the host.
    #[test]
    fn an_embedded_app_takes_its_region_input_and_leaves_tab_chords_to_the_host() {
        let (app, clicks) = clicks_app();
        let mut embedded = super::super::EmbeddedApp::new(app, |_| {}).unwrap();
        let mut taffy = TaffyTree::new();
        let mut nodes = HashMap::new();
        let node = embedded.build_layout(&mut taffy, &Theme::default(), &mut nodes);
        taffy
            .compute_layout(
                node,
                taffy::Size {
                    width: taffy::AvailableSpace::Definite(320.0),
                    height: taffy::AvailableSpace::Definite(200.0),
                },
            )
            .unwrap();
        embedded.apply_layout(&taffy, node, Point::new(100.0, 50.0));
        let mut text = TextRenderer::new();
        let mut scene = Scene::new();
        embedded.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut text,
            &Theme::default(),
            &PresenceCtx::empty(),
        );
        assert!(!scene.encoding().is_empty(), "the app paints in the host");

        let mut doc = automerge::AutoCommit::new();
        let modifiers = winit::event::Modifiers::default();
        let mut send = |embedded: &mut super::super::EmbeddedApp,
                        event: WindowEvent,
                        modifiers: &winit::event::Modifiers| {
            let mut handled = false;
            let mut focus = None;
            embedded.handle_event(
                &event,
                &mut crate::widget::EventCtx {
                    scale_factor: 2.0,
                    modifiers,
                    state: crate::widget::Edits::lend(&mut doc),
                    path: &automerge::ROOT,
                    handled: &mut handled,
                    focus_target: &mut focus,
                    node_id: node,
                    taffy: &taffy,
                },
            );
            handled
        };
        let device = winit::event::DeviceId::dummy();
        send(
            &mut embedded,
            WindowEvent::CursorMoved {
                device_id: device,
                position: (280.0, 160.0).into(),
            },
            &modifiers,
        );
        assert!(send(
            &mut embedded,
            WindowEvent::MouseInput {
                device_id: device,
                state: ElementState::Pressed,
                button: winit::event::MouseButton::Left,
            },
            &modifiers,
        ));
        assert_eq!(
            *clicks.borrow(),
            vec![Point::new(40.0, 30.0)],
            "the click lands at the app's own coordinates"
        );

        let control = winit::event::Modifiers::from(winit::keyboard::ModifiersState::CONTROL);
        let close = crate::synthetic_input::key_event(
            &winit::keyboard::Key::Character(winit::keyboard::SmolStr::new("w")),
            ElementState::Pressed,
        );
        assert!(
            !send(&mut embedded, close, &control),
            "Ctrl+W is the host's to close its tab"
        );
    }

    /// A screen reader walks from the host into an embedded app, finding its
    /// controls where they are on the host's screen, and keyboard focus
    /// enters the app from the host and leaves it at the end of its order.
    #[test]
    fn an_embedded_app_is_in_the_hosts_accessibility_tree_and_focus_order() {
        let (app, _clicks) = clicks_app();
        let mut embedded = super::super::EmbeddedApp::new(app, |_| {}).unwrap();
        let mut taffy = TaffyTree::new();
        let mut nodes = HashMap::new();
        let node = embedded.build_layout(&mut taffy, &Theme::default(), &mut nodes);
        taffy
            .compute_layout(
                node,
                taffy::Size {
                    width: taffy::AvailableSpace::Definite(320.0),
                    height: taffy::AvailableSpace::Definite(200.0),
                },
            )
            .unwrap();
        embedded.apply_layout(&taffy, node, Point::new(100.0, 50.0));
        embedded.draw(
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            &Theme::default(),
            &PresenceCtx::empty(),
        );

        fn find(node: &crate::a11y::AccessNode, name: &str) -> Option<crate::a11y::AccessNode> {
            if node.name.as_deref() == Some(name) {
                return Some(node.clone());
            }
            node.children.iter().find_map(|child| find(child, name))
        }
        let tree = embedded.accessibility().expect("the tile is in the tree");
        let press = find(&tree, "Press").expect("the app's control is below the tile");
        let bounds = press.bounds.expect("placed on the host's screen").to_rect();
        assert_eq!(bounds.origin(), Point::new(100.0, 50.0));

        embedded.set_focus(true);
        assert!(
            embedded.app().focused_control().is_some(),
            "focus entering the app lands on its first control"
        );
        let mut doc = automerge::AutoCommit::new();
        let mut handled = false;
        let mut focus = None;
        embedded.handle_event(
            &crate::synthetic_input::key_event(
                &winit::keyboard::Key::Named(winit::keyboard::NamedKey::Tab),
                ElementState::Pressed,
            ),
            &mut crate::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &winit::event::Modifiers::default(),
                state: crate::widget::Edits::lend(&mut doc),
                path: &automerge::ROOT,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: node,
                taffy: &taffy,
            },
        );
        assert!(
            !handled && embedded.app().focused_control().is_none(),
            "Tab from the app's last control leaves it for the host"
        );
        embedded.set_focus(false);
        embedded.set_focus(true);
        assert!(embedded.app().focused_control().is_some());
    }
}
