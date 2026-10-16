//! Where an [`App`] is shown: a window of its own or a region of a host's
//! surface. The app's input, focus, palette and presence handling run the
//! same way on either; only what a surface itself provides — its scale, its
//! size, a redraw, the pointer's shape — comes from the surface.

use super::*;
use crate::profiling::InputPhase;

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
    pub fn surface_input(
        &mut self,
        window: &dyn AppSurface,
        event: WindowEvent,
        synthesized_touch: bool,
    ) -> bool {
        // Controls swapped in or moved under a still pointer learn it is
        // there before the press or release that follows.
        if let Some(moved) = self.input.pointer_replay.before(&event) {
            self.dispatch_surface_input(window, moved, synthesized_touch);
        }
        let ends_press = crate::pointer_replay::PointerReplay::ends_press(&event);
        let handled = self.dispatch_surface_input(window, event, synthesized_touch);
        if ends_press {
            self.input.pointer_replay.end_press();
        }
        handled
    }

    /// [`Self::route_surface_input`], marking the semantic tree stale for
    /// any input but a pointer move that nothing took up: one no widget
    /// handled, wants to repaint or relayout for, over no floating surface,
    /// and that marked nothing itself, changed nothing a widget declares
    /// (the pointer's own position is no part of the tree). The tree is
    /// stale while the input is routed, for anything that reads it then.
    fn dispatch_surface_input(
        &mut self,
        window: &dyn AppSurface,
        event: WindowEvent,
        synthesized_touch: bool,
    ) -> bool {
        let stale = self.accessibility.semantic_tree_stale;
        self.mark_semantic_tree_stale();
        let marks = self.accessibility.semantic_tree_marks;
        let pointer_move = matches!(event, WindowEvent::CursorMoved { .. });
        let handled = self.route_surface_input(window, event, synthesized_touch);
        let inert = pointer_move
            && !handled
            && self.accessibility.semantic_tree_marks == marks
            && !self.unsettled()
            && !self.root_widget.needs_redraw()
            && !self.history_needs_redraw()
            && self.root_widget.floating_surface_presentations().is_empty();
        if inert {
            self.accessibility.semantic_tree_stale = stale;
        }
        handled
    }

    #[allow(
        clippy::too_many_lines,
        reason = "input routing shares mutable state and must retain fixed precedence"
    )]
    fn route_surface_input(
        &mut self,
        window: &dyn AppSurface,
        event: WindowEvent,
        synthesized_touch: bool,
    ) -> bool {
        let _entry = self.enter_wake_scope();
        self.track_pointer_buttons(&event);
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
        // Splits this event's cost into the phases a slow input stream has
        // to be attributed to; reads no clock unless profiling is on.
        let mut phases = crate::profiling::InputPhaseClock::new(__prof_input.is_some());
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
            let publication = phases.start();
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
            phases.finish(InputPhase::Publication, publication);
        }

        let mut handled = false;
        let mut view_only_scroll = false;
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
            let resources = phases.start();
            app.refresh_controls_before_input(&event);
            phases.finish(InputPhase::Resources, resources);
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
            let resources = phases.start();
            let resources_before = app.resource_input_mark(&event);
            phases.finish(InputPhase::Resources, resources);
            let dispatch = phases.start();
            history_handled |= app.presentation_handle_event(
                event_for_root,
                scale_factor,
                &mut handled,
                &mut focus_target,
            );
            phases.finish(InputPhase::Dispatch, dispatch);
            // The event may have replaced widgets (a dialog closing on its
            // own button): forget their nodes before the fallbacks below look
            // the focused node up. A focus target named by the old map is
            // gone with it; the next frame lays the new widgets out.
            if app.forget_replaced_widgets() {
                focus_target = None;
                window.request_redraw();
            }
            let presentation_is_floating =
                !app.root_widget.floating_surface_presentations().is_empty();
            // Even a document-free interaction may switch which resources a
            // widget presents (a tab, open media, a local mode), so the tree
            // is dropped once here at the mutable tree boundary and every
            // later consumer in this event/frame shares the rebuilt one. Bare
            // hover that changed nothing keeps it: rebuilding the whole tree
            // per pointer move cost a full publication at the report rate.
            let resources = phases.start();
            view_only_scroll = app.invalidate_resources_after_input(
                &event,
                handled
                    || history_handled
                    || focus_target.is_some()
                    || presentation_was_floating != presentation_is_floating,
                &resources_before,
                !history_handled
                    && focus_target.is_none()
                    && presentation_was_floating == presentation_is_floating,
            );
            phases.finish(InputPhase::Resources, resources);
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
                            widget.perform_action(
                                &act,
                                &[],
                                &mut crate::widget::Edits::lend(doc),
                                root,
                            )
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

        let publication = phases.start();
        app.broadcast_focus_presence();
        app.broadcast_location_presence();
        app.broadcast_view_presence();
        app.publish_action_surface();
        app.publish_continuation_metadata();
        app.publish_playback_handoff();
        phases.finish(InputPhase::Publication, publication);

        if let Some(id) = focus_target {
            app.set_focused_node(Some(id));
            handled = true;
        }

        let publication = phases.start();
        app.announce_state_change(state_pre);
        app.announce_text_review(review_pre);
        phases.finish(InputPhase::Publication, publication);

        if handled {
            let commit = phases.start();
            if view_only_scroll && !history_handled && focus_target.is_none() {
                app.commit_action_with_invalidation(false);
            } else {
                app.finalize_handled_native_event(&event, is_undo_redo, history_handled);
                app.needs_sync = true;
            }
            phases.finish(InputPhase::Commit, commit);
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
            let hit_test = phases.start();
            let desired = app.resolve_cursor(logical).unwrap_or(CursorIcon::Default);
            let target = app.tooltip_target_at(logical);
            phases.finish(InputPhase::HitTest, hit_test);
            if app.input.current_cursor != desired {
                app.input.current_cursor = desired;
                window.set_cursor(desired);
            }
            // `on_pointer` keeps the dwell running over the same
            // region and restarts it on a new one.
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
            p.record_input(start, start.elapsed(), phases.phases());
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
        self.surface_frame_with_damage(surface, scene, &mut SurfaceDamage::default())
    }

    /// [`Self::surface_frame`], adding to `damage` where this frame's pixels
    /// differ from the last frame's, as a window of its own submits them.
    pub fn surface_frame_with_damage(
        &mut self,
        surface: &dyn AppSurface,
        scene: &mut Scene,
        damage: &mut SurfaceDamage,
    ) -> bool {
        let _entry = self.enter_wake_scope();
        // Armed for this frame: each rebuild it does clears it unless every
        // request behind it was a change the widgets describe.
        self.layout.frame_rebuilds_described = true;
        let replaced = self.swap_requested_component();
        // The model's own work, as a hosted worker's pump runs it:
        // collaboration (inbound edits, directory requests, publication),
        // invocations, idle model work and the record. A different record
        // is a different document under the same widgets.
        let record_changed = self.pump_hosted_model_record_changed();
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
        // A rebuild the widgets describe (only the reporting widget replaced
        // children, and its damage covers them) keeps the frame's damage: the
        // widgets' own, and every widget box that came, went or moved. Any
        // other reason to rebuild this frame disarms it.
        let changed = self.take_root_structure_dirty();
        if replaced {
            self.layout.request_rebuild();
        }
        self.layout.structure_dirty |= changed;
        let structure_rebuilt = self.layout.structure_dirty;
        // Every widget and its box as the last frame left them, before
        // anything is rebuilt: compared by identity only, never reached.
        let boxes_before = widget_boxes(self.root_widget.as_ref());
        if structure_rebuilt {
            self.rebuild_structure();
        }
        let Some(root_node) = self.layout.root_node else {
            damage.repaint("no layout");
            return true;
        };
        let rebuilds = self.layout.rebuilds;
        let Some((geometry_changed, _, _)) = run_layout_pass(
            self,
            root_node,
            size.width,
            size.height,
            scale_factor,
            structure_rebuilt,
            None,
        ) else {
            damage.repaint("no layout");
            return true;
        };
        if geometry_changed {
            self.input.pointer_replay.mark_unsettled();
        }
        let search_pending = self
            .actions
            .action_palette
            .as_mut()
            .is_some_and(|palette| palette.advance_search_page().pending);
        // What a window of its own repaints whole: a rebuilt tree, moved
        // geometry, another record, and what paints over the widgets.
        // Carets, selections and marks widgets paint from presence carry no
        // widget damage (`draw_presence_layer`).
        let presence_changed =
            damage.widgets_presence_changed(self.presence_ctx.widget_presence(crate::time::now()));
        let hud = self
            .profiler
            .as_ref()
            .is_some_and(crate::profiling::FrameProfiler::hud_enabled);
        // A relayout that moved some of the same widgets repaints where each
        // was and is: what else is there is drawn by widgets that did not
        // move, and their own damage covers any change to them.
        // A layout pass that recomposed the tree may have replaced widgets.
        let recomposed = self.layout.rebuilds != rebuilds;
        let described = (structure_rebuilt || recomposed) && self.layout.frame_rebuilds_described;
        let relaid = !described && !recomposed && geometry_changed && !structure_rebuilt;
        let boxes_after = (described || relaid).then(|| widget_boxes(self.root_widget.as_ref()));
        let moved = match &boxes_after {
            Some(after) if described => Some(changed_boxes(&boxes_before, after)),
            Some(after) => moved_boxes(&boxes_before, after),
            None if recomposed => None,
            None => Some(Vec::new()),
        };
        let full = [
            (replaced, "another component"),
            (record_changed, "another record"),
            (structure_rebuilt && !described, "a rebuilt tree"),
            (moved.is_none(), "moved geometry"),
            (self.debug_mode, "debug outlines"),
            (self.actions.action_palette.is_some(), "the action palette"),
            (hud, "the profiler HUD"),
            (presence_changed, "presence marks"),
        ]
        .into_iter()
        .find_map(|(repaint, why)| repaint.then_some(why));
        let drawn = draw_frame_content(self, scene, size.width, size.height, scale_factor, None);
        // Collaborators' marks over the widgets, for a host that shows the
        // app as a window of its own does ([`SurfaceDamage::painting_peers`]).
        let surface_rect = KurboRect::new(0.0, 0.0, f64::from(size.width), f64::from(size.height));
        let mut peer_layer = damage.peers.take();
        let peers = peer_layer.as_deref_mut().map(|peers| {
            let changed = draw_presence_layer(
                self,
                &mut peers.layer,
                drawn.transform,
                drawn.window_rect,
                crate::time::now(),
            );
            scene.append(peers.layer.scene(), None);
            PeerFrame {
                changed,
                marks: peer_marks(&peers.layer, &mut peers.bounds, surface_rect),
            }
        });
        let marks_unbounded = peers.as_ref().is_some_and(|peers| peers.marks.is_none());
        // A widget whose own damage this draw covers where its box was and is
        // (a block editor that grew by a paragraph) leaves its box to that
        // damage; its children's boxes still count.
        let covered = match (&moved, &boxes_after) {
            (Some(moved), Some(_)) if !moved.is_empty() => {
                boxes_covered_by_own_damage(self.root_widget.as_ref())
            }
            _ => std::collections::HashSet::new(),
        };
        let uncovered = |boxes: &[(WidgetIdentity, KurboRect)]| -> Vec<_> {
            boxes
                .iter()
                .filter(|(widget, _)| !covered.contains(widget))
                .copied()
                .collect()
        };
        let moved = match (moved, &boxes_after) {
            (Some(moved), Some(after)) if !moved.is_empty() => {
                let (before, after) = (uncovered(&boxes_before), uncovered(after));
                if covered.is_empty() {
                    Some(moved)
                } else if described {
                    Some(changed_boxes(&before, &after))
                } else {
                    moved_boxes(&before, &after).or(Some(moved))
                }
            }
            (moved, _) => moved,
        };
        // What moved besides what scrolls copy, and what came or went in a
        // scrolled content; `None` when a rebuild no widget described moved
        // what cannot be said.
        let moved_besides = |offers: &[crate::scroll_copy::ScrollOffer]| match &boxes_after {
            Some(after) if described || relaid => {
                let moves = crate::scroll_copy::moves_besides(
                    &uncovered(&boxes_before),
                    &uncovered(after),
                    offers,
                );
                Some([moves.besides, moves.carried].concat())
            }
            _ if recomposed || structure_rebuilt => None,
            _ => Some(Vec::new()),
        };
        let copying = damage.copying;
        let scale = f64::from(scale_factor);
        let full = full.or_else(|| {
            peers
                .as_ref()
                .is_some_and(|peers| peers.changed.is_none())
                .then_some("presence marks")
        });
        let plan_scale = (scale, damage.copy_scale);
        let plan_size = (
            device_extent(size.width, damage.copy_scale),
            device_extent(size.height, damage.copy_scale),
        );
        damage.frame(full, &drawn, scale, peers.as_ref(), |regions| {
            let root = self.root_widget.as_ref();
            if !copying {
                collect_submitted_widget_damage(root, regions);
                regions.extend(moved.into_iter().flatten());
                return crate::scroll_copy::CopyPlan::default();
            }
            let offers =
                crate::scroll_copy::accepting(|| collect_submitted_widget_damage(root, regions));
            if offers.is_empty() {
                regions.extend(moved.into_iter().flatten());
                return crate::scroll_copy::CopyPlan::default();
            }
            // What is painted over the widgets (a menu, the palette, a
            // tooltip, peer marks that cannot be bounded) does not move with
            // a scroll: the scrollers repaint.
            if drawn.painted_over || marks_unbounded {
                regions.extend(moved.into_iter().flatten());
                regions.extend(offers.iter().map(crate::scroll_copy::ScrollOffer::viewport));
                return crate::scroll_copy::CopyPlan::default();
            }
            let Some(besides) = moved_besides(&offers) else {
                regions.extend(moved.into_iter().flatten());
                regions.extend(offers.iter().map(crate::scroll_copy::ScrollOffer::viewport));
                return crate::scroll_copy::CopyPlan::default();
            };
            regions.extend(besides);
            let history = self
                .history
                .as_deref()
                .map(|history| history as &dyn Widget);
            crate::scroll_copy::plan_scaled(
                &offers,
                &std::iter::once(root).chain(history).collect::<Vec<_>>(),
                plan_scale,
                plan_size,
            )
        });
        if let Some(mut peers) = peer_layer {
            peers.layer.presented();
            damage.peers = Some(peers);
        }
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

/// A widget's identity within one tree walk: its address and its depth (a
/// widget held at the start of its parent shares the parent's address).
type WidgetIdentity = (*const (), usize);

/// Every widget in tree order, by identity, with its layout box.
fn widget_boxes(root: &dyn Widget) -> Vec<(WidgetIdentity, KurboRect)> {
    fn walk(widget: &dyn Widget, depth: usize, out: &mut Vec<(WidgetIdentity, KurboRect)>) {
        out.push((
            (std::ptr::from_ref(widget).cast::<()>(), depth),
            widget.get_rect(),
        ));
        widget.for_each_child(&mut |child| walk(child, depth.saturating_add(1), out));
    }
    let mut out = Vec::new();
    walk(root, 0, &mut out);
    out
}

/// Every widget whose own damage covers a change to its box this frame
/// ([`Widget::damage_covers_own_box`]), by identity as [`widget_boxes`] has it.
fn boxes_covered_by_own_damage(root: &dyn Widget) -> std::collections::HashSet<WidgetIdentity> {
    fn walk(
        widget: &dyn Widget,
        depth: usize,
        out: &mut std::collections::HashSet<WidgetIdentity>,
    ) {
        if widget.damage_covers_own_box() {
            out.insert((std::ptr::from_ref(widget).cast::<()>(), depth));
        }
        widget.for_each_child(&mut |child| walk(child, depth.saturating_add(1), out));
    }
    let mut out = std::collections::HashSet::new();
    walk(root, 0, &mut out);
    out
}

/// The old and new box of every widget a relayout moved or resized, when
/// `before` and `after` list the same widgets in the same order; `None`
/// when they do not, and which pixels moved cannot be said.
fn moved_boxes(
    before: &[(WidgetIdentity, KurboRect)],
    after: &[(WidgetIdentity, KurboRect)],
) -> Option<Vec<KurboRect>> {
    if before.len() != after.len() {
        return None;
    }
    let mut moved = Vec::new();
    for ((old_widget, old), (new_widget, new)) in before.iter().zip(after) {
        if old_widget != new_widget {
            return None;
        }
        if old != new {
            moved.extend([*old, *new]);
        }
    }
    Some(moved)
}

/// The old and new box of every widget that a rebuild and relayout moved,
/// and the box of every widget that came or went, matched by identity.
fn changed_boxes(
    before: &[(WidgetIdentity, KurboRect)],
    after: &[(WidgetIdentity, KurboRect)],
) -> Vec<KurboRect> {
    let old: std::collections::HashMap<_, _> = before.iter().copied().collect();
    let new: std::collections::HashMap<_, _> = after.iter().copied().collect();
    let mut changed = Vec::new();
    for (widget, rect) in after {
        match old.get(widget) {
            Some(was) if was == rect => {}
            Some(was) => changed.extend([*was, *rect]),
            None => changed.push(*rect),
        }
    }
    changed.extend(
        before
            .iter()
            .filter(|(widget, _)| !new.contains_key(widget))
            .map(|(_, rect)| *rect),
    );
    changed
}

/// `extent` scene pixels in device pixels at `scale` per scene pixel.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a surface's extent at a display's scale is a small non-negative pixel count"
)]
fn device_extent(extent: u32, scale: f64) -> u32 {
    (f64::from(extent) * scale).round().max(0.0) as u32
}

/// The collaborators' layer a surface paints over the widgets as a window
/// of its own does ([`SurfaceDamage::painting_peers`]).
#[derive(Default)]
struct PeerLayer {
    layer: crate::presence::PresenceLayer,
    bounds: crate::scene_parts::DrawBounds,
}

/// What the collaborators' layer did in one frame: its damage against the
/// frame before (`None` when it cannot be proven), and where it paints, on
/// screen and now (`None` when that cannot be bounded), in scene pixels.
struct PeerFrame {
    changed: Option<Vec<KurboRect>>,
    marks: Option<crate::presence::LayerMarks>,
}

/// Where an app's frames on a surface changed since its host last looked
/// ([`Self::take`]), decided as a window of its own decides what to submit:
/// the widgets' damage and the tooltip's bounds, unless something no damage
/// describes changed, which repaints the surface.
pub struct SurfaceDamage {
    /// In the surface's scene coordinates. `None` once a frame since the
    /// last take could not describe its change, and before the first frame.
    pending: Option<Vec<KurboRect>>,
    /// The last frame painted a layer over the widgets (an overlay, history
    /// chrome, a dialog, the palette) that their damage does not describe.
    top_layer_drawn: bool,
    /// Where the last frame's tooltip painted.
    tooltip: Option<KurboRect>,
    /// The presence the last frame's widgets painted from.
    widgets: Option<crate::presence::WidgetPresence>,
    /// Why the last frame repainted the surface whole, if it did.
    repainted: Option<&'static str>,
    /// Whether the host copies what a scroll keeps on screen
    /// ([`Self::copying`]).
    copying: bool,
    /// The copies of the frames since the last take, in device pixels at
    /// [`Self::copy_scale`] per scene pixel, to make before repainting the
    /// damage.
    copies: Vec<crate::scroll_copy::DeviceCopy>,
    /// How many pixels of the host's target one pixel of the surface's
    /// scene is ([`Self::set_copy_scale`]).
    copy_scale: f64,
    /// The collaborators' layer, when the surface paints it.
    peers: Option<Box<PeerLayer>>,
}

impl Default for SurfaceDamage {
    fn default() -> Self {
        Self {
            pending: None,
            top_layer_drawn: false,
            tooltip: None,
            widgets: None,
            repainted: None,
            copying: false,
            copies: Vec::new(),
            copy_scale: 1.0,
            peers: None,
        }
    }
}

impl SurfaceDamage {
    /// For a host that presents each frame over the pixels of the last one
    /// and makes the copies [`Self::take_copying`] returns before it repaints
    /// the damage: a scroll then copies what stays on screen instead of
    /// damaging its viewport ([`crate::scroll_copy`]).
    pub fn copying() -> Self {
        Self {
            copying: true,
            ..Self::default()
        }
    }

    /// Whether this is for a host that copies ([`Self::copying`]).
    pub fn is_copying(&self) -> bool {
        self.copying
    }

    /// Paint collaborators' pointers, names and activity over the widgets,
    /// as a window of its own does, and damage only the marks that changed;
    /// a scroll beneath them still copies, repainting the marks where they
    /// are and where its copy took them. For a host that does not paint
    /// them itself (an embedded app's host does).
    #[must_use]
    pub fn painting_peers(mut self) -> Self {
        self.peers = Some(Box::default());
        self
    }

    /// The host's target has `device_per_unit` pixels per pixel of the
    /// surface's scene (an app drawn into a desktop's logical scene shown at
    /// 200 % has 2): copies are planned in those pixels, so a step of half a
    /// scene pixel there is a whole pixel and copies. The copies
    /// [`Self::take_copying`] returns are in them.
    pub fn set_copy_scale(&mut self, device_per_unit: f64) {
        let scale = if device_per_unit.is_finite() && device_per_unit > 0.0 {
            device_per_unit
        } else {
            1.0
        };
        if scale.to_bits() == self.copy_scale.to_bits() {
            return;
        }
        // Copies not yet taken were planned at the old scale: they land as
        // damage instead.
        let copies = std::mem::take(&mut self.copies);
        let unit = self.copy_scale;
        if let Some(pending) = self.pending.as_mut() {
            pending.extend(copies.iter().map(|copy| {
                let to = copy.destination_rect();
                KurboRect::new(to.x0 / unit, to.y0 / unit, to.x1 / unit, to.y1 / unit)
            }));
        }
        self.copy_scale = scale;
    }

    /// How many pixels of the host's target one pixel of the surface's
    /// scene is ([`Self::set_copy_scale`]).
    pub fn copy_scale(&self) -> f64 {
        self.copy_scale
    }

    /// Where the frames since the last take changed, or `None` when they
    /// must be repainted whole. A copy is damage here: where it would land.
    pub fn take(&mut self) -> Option<Vec<KurboRect>> {
        let copies = std::mem::take(&mut self.copies);
        let mut pending = self.pending.replace(Vec::new())?;
        let unit = self.copy_scale;
        pending.extend(copies.iter().map(|copy| {
            let to = copy.destination_rect();
            KurboRect::new(to.x0 / unit, to.y0 / unit, to.x1 / unit, to.y1 / unit)
        }));
        Some(pending)
    }

    /// The copies to make, in order, and then where the frames since the
    /// last take changed; `None` when they must be repainted whole.
    pub fn take_copying(
        &mut self,
    ) -> Option<(Vec<crate::scroll_copy::DeviceCopy>, Vec<KurboRect>)> {
        let copies = std::mem::take(&mut self.copies);
        self.pending
            .replace(Vec::new())
            .map(|pending| (copies, pending))
    }

    /// Why the last frame repainted the surface whole, or `None` when its
    /// damage described it.
    pub fn repainted(&self) -> Option<&'static str> {
        self.repainted
    }

    fn repaint(&mut self, why: &'static str) {
        self.pending = None;
        self.copies.clear();
        self.repainted = Some(why);
    }

    /// Whether what widgets paint from presence changed, or follows their
    /// content (a peer's caret or selection, fresh typing): either moves
    /// marks no widget reports.
    fn widgets_presence_changed(&mut self, widgets: crate::presence::WidgetPresence) -> bool {
        let changed = widgets.marks_follow_content() || self.widgets.as_ref() != Some(&widgets);
        self.widgets = Some(widgets);
        changed
    }

    /// Add one drawn frame: repainted whole for the reason `full` gives,
    /// when a layer is painted over the widgets or was last frame, and
    /// otherwise the widget damage `collect` reports (logical, scaled by
    /// `scale` to the scene) with the tooltip where it was and where it is,
    /// and the copies and damage of the scrolls it planned (in the scene).
    fn frame(
        &mut self,
        full: Option<&'static str>,
        drawn: &DrawnFrame,
        scale: f64,
        peers: Option<&PeerFrame>,
        collect: impl FnOnce(&mut Vec<KurboRect>) -> crate::scroll_copy::CopyPlan,
    ) {
        let layer_shown = std::mem::replace(&mut self.top_layer_drawn, drawn.top_layer.is_some());
        let tooltip_shown = std::mem::replace(&mut self.tooltip, drawn.tooltip);
        let full = full
            .or(drawn.top_layer)
            .or_else(|| layer_shown.then_some("a layer gone from over the widgets"));
        if let Some(why) = full {
            self.repaint(why);
            return;
        }
        self.repainted = None;
        let Some(pending) = self.pending.as_mut() else {
            return;
        };
        let mut regions = Vec::new();
        let planned = collect(&mut regions);
        // Planned, carried and kept in the host's device pixels; pending in
        // the scene's.
        let unit = self.copy_scale;
        let to_device = Affine::scale(unit);
        let mut changed: Vec<KurboRect> = regions
            .into_iter()
            .map(|rect| Affine::scale(scale * unit).transform_rect_bbox(rect))
            .collect();
        // The tooltip paints over the widgets: a label that changes in
        // place has no other damage. Nor do the collaborators' marks.
        let peer_changed = peers
            .and_then(|peers| peers.changed.as_deref())
            .unwrap_or_default()
            .iter()
            .map(|rect| rect.expand().inflate(1.0, 1.0));
        changed.extend(
            tooltip_shown
                .into_iter()
                .chain(drawn.tooltip)
                .chain(peer_changed)
                .map(|rect| to_device.transform_rect_bbox(rect)),
        );
        changed.extend(planned.damage);
        // A copy reads the pixels the last take left. After another frame
        // since, those are not all there: that frame's damage is repainted
        // only once the host takes, so the copy lands as damage instead.
        if pending.is_empty() && self.copies.is_empty() {
            crate::scroll_copy::carry(&planned.copies, &mut changed);
            if let Some(marks) = peers.and_then(|peers| peers.marks.as_ref()) {
                let device = |rects: &[KurboRect]| -> Vec<KurboRect> {
                    rects
                        .iter()
                        .map(|rect| to_device.transform_rect_bbox(*rect))
                        .collect()
                };
                crate::scroll_copy::marks_over_copies(
                    &planned.copies,
                    &device(&marks.shown),
                    &device(&marks.painted),
                    &mut changed,
                );
            }
            changed.extend(planned.exposed);
            self.copies.extend(planned.copies);
        } else {
            changed.extend(planned.exposed);
            changed.extend(
                planned
                    .copies
                    .iter()
                    .map(crate::scroll_copy::DeviceCopy::destination_rect),
            );
        }
        let to_scene = Affine::scale(unit.recip());
        pending.extend(
            changed
                .into_iter()
                .map(|rect| to_scene.transform_rect_bbox(rect)),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Dispatch;
    use crate::widget::TaffyTreeExt;
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    thread_local! {
        static CLICKS: RefCell<Option<Rc<RefCell<Vec<Point>>>>> = const { RefCell::new(None) };
        static PHYSICAL: RefCell<Vec<crate::widget::PhysicalInput>> = const { RefCell::new(Vec::new()) };

    }

    include!("native_publication_scroll_tests.rs");
    include!("semantic_publication_tests.rs");

    #[cfg(feature = "native")]
    mod input_phase_profile {
        use super::*;
        include!("input_phase_profile_tests.rs");
    }

    #[test]
    fn a_relayout_repaints_where_moved_widgets_were_and_are() {
        let (a, b) = (1_u8, 2_u8);
        let (a, b) = (
            (std::ptr::from_ref(&a).cast::<()>(), 1),
            (std::ptr::from_ref(&b).cast::<()>(), 1),
        );
        let field = KurboRect::new(0.0, 0.0, 100.0, 20.0);
        let label = KurboRect::new(0.0, 30.0, 50.0, 40.0);
        let grown = KurboRect::new(0.0, 0.0, 140.0, 20.0);
        assert_eq!(
            moved_boxes(&[(a, field), (b, label)], &[(a, grown), (b, label)]),
            Some(vec![field, grown])
        );
        assert_eq!(
            moved_boxes(&[(a, field), (b, label)], &[(a, field), (b, label)]),
            Some(Vec::new())
        );
        // Other widgets, or more of them: which pixels moved is unknown.
        assert_eq!(
            moved_boxes(&[(a, field), (b, label)], &[(b, field), (a, label)]),
            None
        );
        assert_eq!(moved_boxes(&[(a, field)], &[(a, field), (b, label)]), None);
    }

    #[test]
    fn a_described_rebuild_repaints_the_widgets_that_came_went_or_moved() {
        let (a, b, c) = (1_u8, 2_u8, 3_u8);
        let [a, b, c] = [&a, &b, &c].map(|widget| (std::ptr::from_ref(widget).cast::<()>(), 1));
        let header = KurboRect::new(0.0, 0.0, 100.0, 20.0);
        let chip = KurboRect::new(0.0, 0.0, 40.0, 20.0);
        let body = KurboRect::new(0.0, 30.0, 100.0, 90.0);
        let lower = KurboRect::new(0.0, 40.0, 100.0, 100.0);
        // A chip came, the body moved down under it; the header kept its box.
        assert_eq!(
            changed_boxes(
                &[(a, header), (b, body)],
                &[(a, header), (c, chip), (b, lower)]
            ),
            vec![chip, body, lower]
        );
        // The chip went again.
        assert_eq!(
            changed_boxes(
                &[(a, header), (c, chip), (b, body)],
                &[(a, header), (b, body)]
            ),
            vec![chip]
        );
        // A widget held at the start of its parent shares the parent's
        // address, not its depth: neither is mistaken for the other.
        let child = (a.0, 2);
        assert_eq!(
            changed_boxes(&[(a, header), (child, chip)], &[(a, header), (child, chip)]),
            Vec::<KurboRect>::new()
        );
    }

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
            taffy.fill_leaf(nodes, self)
        }
        crate::rect_geometry!();
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
                    self.pointer = ctx.logical(position);
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
        let event = crate::synthetic_input::keyboard_input(winit::event::KeyEvent::synthetic(
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
        ));
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
        app.layout.request_rebuild();
        app.layout.layout_dirty = true;
        app.surface_frame(&region, &mut Scene::new());
        PHYSICAL.with_borrow_mut(Vec::clear);
        app.surface_input(
            &region,
            crate::synthetic_input::keyboard_input(winit::event::KeyEvent::synthetic(
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
            )),
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

        let modifiers = winit::event::Modifiers::default();
        let send = |embedded: &mut super::super::EmbeddedApp,
                    event: WindowEvent,
                    modifiers: &winit::event::Modifiers| {
            Dispatch::new()
                .scale_factor(2.0)
                .modifiers(*modifiers)
                .layout(&taffy, node)
                .send(embedded, &event)
                .handled
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
        let close = crate::synthetic_input::key_input(
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
        let handled = Dispatch::new()
            .layout(&taffy, node)
            .send(
                &mut embedded,
                &crate::synthetic_input::key_input(
                    &winit::keyboard::Key::Named(winit::keyboard::NamedKey::Tab),
                    ElementState::Pressed,
                ),
            )
            .handled;
        assert!(
            !handled && embedded.app().focused_control().is_none(),
            "Tab from the app's last control leaves it for the host"
        );
        embedded.set_focus(false);
        embedded.set_focus(true);
        assert!(embedded.app().focused_control().is_some());
    }
}
