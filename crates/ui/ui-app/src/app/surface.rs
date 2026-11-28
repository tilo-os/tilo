//! Where an [`App`] is shown: a window of its own or a region of a host's
//! surface. The app's input, focus, palette and presence handling run the
//! same way on either; only what a surface itself provides — its scale, its
//! size, a redraw, the pointer's shape — comes from the surface.

use super::*;
use crate::profiling::InputPhase;
use tilo_ui_base::{
    damage::WidgetIdentity, damage::boxes_covered_by_own_damage, damage::changed_boxes,
    damage::widget_boxes,
};

/// What an [`App`] needs from the surface it is shown on.
pub trait AppSurface {
    /// Physical pixels per logical pixel.
    fn scale_factor(&self) -> f64;
    fn inner_size(&self) -> winit::dpi::PhysicalSize<u32>;
    /// Ask for another frame.
    fn request_redraw(&self);
    /// Show `cursor` while the pointer is over the app.
    fn set_cursor(&self, cursor: CursorIcon);
    /// Whether the surface's host answers the shell's tab chords (Ctrl+T,
    /// Ctrl+W, Ctrl+Shift+T, Ctrl+Tab, Ctrl+Shift+Tab) itself, so the app leaves them unhandled for it
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
    /// Hand `event` to content the app shows on a card of its own (a call's card the shell stands
    /// over the windows) through `deliver`, at the node the app's widget stands at, and take up
    /// what it did as a press in the window would be. A card is the app's own content on another
    /// surface: its input runs in the app as the window's does. Answers whether it took the event.
    pub(crate) fn dispatch_to_card(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        deliver: impl FnOnce(
            &mut dyn Widget,
            &WindowEvent,
            &mut tilo_ui_base::widget::EventCtx<'_>,
        ) -> bool,
    ) -> bool {
        let _entry = self.enter_wake_scope();
        let Some(root_node) = self.live_app_node() else {
            return false;
        };
        let mut handled = false;
        let mut focus_target = None;
        self.mark_semantic_tree_stale();
        self.input.pointer.track_buttons(event);
        self.refresh_controls_before_input(event);
        let resources_before = self.resource_input_mark(event);
        #[cfg(feature = "model")]
        let capture = self.begin_local_input_event(event);
        let took = deliver(
            self.root_widget.as_mut(),
            event,
            &mut tilo_ui_base::widget::EventCtx {
                scale_factor: scale,
                modifiers: &self.input.modifiers,
                state: tilo_ui_base::widget::Edits::lend(&mut self.state.doc),
                path: &self.state.root,
                handled: &mut handled,
                focus_target: &mut focus_target,
                node_id: root_node,
                taffy: &self.layout.taffy,
            },
        );
        #[cfg(feature = "model")]
        {
            drop(capture);
            self.drain_local_input_capture();
        }
        // `deliver` may change presentation-only resource declarations without touching the
        // CRDT (selected tab, open media, local mode); only bare hover that changed nothing keeps
        // the published tree.
        self.invalidate_resources_after_input(
            event,
            handled || focus_target.is_some(),
            &resources_before,
            false,
        );
        if let Some(id) = focus_target {
            self.focus_for_input(id, event);
        }
        if handled {
            self.commit_action();
            self.needs_sync = true;
            self.layout.layout_dirty |= self.root_widget.is_layout_dirty();
        }
        took || handled
    }

    /// Restore the real focused native configuration after modal/page
    /// replacement, without replaying any key press or typed input.
    pub(super) fn restore_physical_configuration(&mut self) {
        if self.close_admission.dialog.is_none()
            && self.app_live()
            && let Some(layout) = self.input.keyboard_layout.clone()
        {
            self.root_widget
                .observe_physical_input(tilo_ui_base::widget::PhysicalInput::Layout(Some(layout)));
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
        // What the shell wrote into this window since the last frame (a press in its bar, a
        // control of its content a tool or an assistive technology pressed) is run before the
        // input that follows it: what a bar's New event opens is where the person's typing lands,
        // in the order they made them, never a frame behind.
        #[cfg(feature = "native")]
        if matches!(
            &event,
            WindowEvent::KeyboardInput {
                event: winit::event::KeyEvent {
                    state: ElementState::Pressed,
                    ..
                },
                ..
            } | WindowEvent::Ime(winit::event::Ime::Commit(_))
                | WindowEvent::MouseInput {
                    state: ElementState::Pressed,
                    ..
                }
                | WindowEvent::Touch(winit::event::Touch {
                    phase: winit::event::TouchPhase::Started,
                    ..
                })
        ) && self.floating.client.is_some()
            && self.pump_floating_surface_service()
        {
            self.invalidate_widget_resource_cache();
            self.layout.layout_dirty = true;
            // The press's own work (its part event, an opened sheet) waits for the app's sync as
            // it would in a frame: take it now.
            self.sync_model(false);
        }
        // A key typed before the window's content arrived waits for it ([`super::held_input`]).
        let Some(event) = self.queue_behind_held_input(event) else {
            return true;
        };
        // Input framework chrome took the last one runs no queued control until the person's
        // next input: the entry (or return) it asked for has sealed or discarded what was
        // queued before it ([`Self::presentation_handle_event`]).
        self.input.passive_input = false;
        let candidate = Self::held_input_candidate(&event);
        crate::widget::context_menu::note_input(&event);
        // What this event opens or closes follows it: a press moves, a key
        // lands at once. A host that shows an app on a surface of its own
        // (the session's shell, its panels and sheets) reaches the toolkit
        // through here, and would otherwise land everything at once.
        tilo_ui_base::motion::note_input(&event);
        // Where the pointer is now, in logical pixels: a control that grows under it (a dock's
        // item) reads it between frames, where no event reaches it.
        if let Some(at) = tilo_ui_base::motion::pointer_of(&event) {
            let scale = window.scale_factor().max(f64::EPSILON);
            tilo_ui_base::motion::note_pointer(vello::kurbo::Point::new(
                at.x / scale,
                at.y / scale,
            ));
        }
        // Controls swapped in or moved under a still pointer learn it is
        // there before the press or release that follows.
        #[cfg(test)]
        let replay_probe = crate::frame_probe::span("input.replay");
        if let Some(moved) = self.input.pointer_replay.before(&event) {
            self.dispatch_surface_input(window, moved, synthesized_touch);
        }
        #[cfg(test)]
        drop(replay_probe);
        let ends_press = crate::pointer_replay::PointerReplay::ends_press(&event);
        let handled = self.dispatch_surface_input(window, event, synthesized_touch);
        if ends_press {
            self.input.pointer_replay.end_press();
        }
        if !handled && let Some(event) = candidate {
            return self.keep_unhandled_input(event);
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
        #[cfg(test)]
        let route_probe = crate::frame_probe::span("input.route");
        let handled = self.route_surface_input(window, event, synthesized_touch);
        #[cfg(test)]
        drop(route_probe);
        #[cfg(feature = "model")]
        {
            self.drain_local_input_capture();
        }
        let inert = pointer_move
            && !handled
            && self.accessibility.semantic_tree_marks == marks
            && !self.unsettled()
            && !(self.root_widget.needs_redraw() || self.root_widget.needs_animation_frame())
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
        // What the event changed reaches widgets that paint it without a
        // flag of their own ([`tilo_ui_base::input_generation`]).
        tilo_ui_base::input_generation::note();
        self.input.pointer.track_buttons(&event);
        if self
            .input
            .pointer
            .track_place(&event, window.scale_factor())
        {
            // The person acts again: what stood at their last control is
            // done with.
            self.dismiss_control_notice();
        }
        // A material stage lights where the pointer is: it moving over the window is a new frame.
        if tilo_ui_base::part_paint::backdrop::follows_pointer()
            && matches!(
                &event,
                WindowEvent::CursorMoved { .. } | WindowEvent::CursorLeft { .. }
            )
        {
            window.request_redraw();
        }
        // A tooltip goes when the person acts, or when the pointer or the
        // keyboard leaves the window, whatever then takes the event: History's
        // controls and the close dialog take some before the widgets do.
        if crate::widget::TooltipHost::dismissed_by(&event) && self.input.tooltips.dismiss() {
            window.request_redraw();
        }
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
            // What the person does lands on the controls the app shows now.
            // A change of the app's structure since its last frame (what it
            // took in while the person typed) left the layout map behind: a
            // press would name a control the map no longer reaches, and the
            // control it leaves would keep the keyboard.
            if self.build_tree_before_input() {
                self.lay_out_before_input(window);
            }
            // A context-menu press or key names the item of the window it lands on (the focused
            // one, for a key), for the menu it opens.
            let pointed = (crate::widget::context_menu::context_gesture()
                && matches!(
                    event,
                    WindowEvent::MouseInput { .. } | WindowEvent::KeyboardInput { .. }
                ))
            .then(|| {
                let at = match event {
                    WindowEvent::MouseInput { .. } => self.input.pointer.pressed_at,
                    _ => self.keyboard_place().map(|place| place.center()),
                }?;
                let chain = crate::pick::targets_at(self.root_widget.as_ref(), at);
                Some((at, crate::shared_selection::item_at(&chain)))
            })
            .flatten();
            crate::shared_selection::set_pointed(pointed);
        }
        match &event {
            WindowEvent::KeyboardLayoutChanged(layout) => {
                self.input.keyboard_layout.clone_from(layout)
            }
            // Every press and click reads the modifiers the surface last reported, whatever
            // source the input comes from.
            WindowEvent::ModifiersChanged(modifiers) => self.input.modifiers = *modifiers,
            WindowEvent::Focused(false) => {
                self.input.keyboard_layout = None;
                self.input.window_focused = false;
                self.input.keyboard_entry_owed = false;
                // A secret the window showed hides itself now, and the frame that showed it goes
                // from the cache with it: it is drawn again, masked.
                self.root_widget.window_focus_changed(false);
                self.root_widget.release_scene_cache();
                // The field the person left keeps its text and selection, and gives up the caret
                // with the keyboard: the frame is drawn again where the caret stood.
                let caret_gone = std::mem::replace(&mut self.input.keyboard_held, false);
                if caret_gone || self.root_widget.needs_redraw() {
                    window.request_redraw();
                }
            }
            // A window that takes the keyboard hands it to the app's entry
            // when nothing in it holds it: what is typed after it opens, or
            // is brought forward, reaches the app.
            WindowEvent::Focused(true) => {
                self.input.window_focused = true;
                // What the window hid when it lost the focus stays hidden until the app sends a
                // new value.
                self.root_widget.window_focus_changed(true);
                if !self.input.keyboard_held {
                    self.input.keyboard_held = true;
                    self.root_widget.release_scene_cache();
                    window.request_redraw();
                }
                self.hand_keyboard_to_app();
            }
            _ => {}
        }
        self.restore_physical_configuration();
        // The chord that turns the screen reader on and off answers on every
        // screen, whatever the window shows and binds.
        if self.handle_screen_reader_chord(&event) {
            window.request_redraw();
            return true;
        }
        if self.close_admission.dialog.is_some() || !self.app_live() {
            self.root_widget
                .observe_physical_input(tilo_ui_base::widget::PhysicalInput::Focused(false));
        } else if let Some(input) = tilo_ui_base::widget::PhysicalInput::from_window_event(&event) {
            self.root_widget.observe_physical_input(input);
        }
        if self.close_handle_event(&event, window.scale_factor()) {
            self.root_widget
                .observe_physical_input(tilo_ui_base::widget::PhysicalInput::Focused(false));
            window.request_redraw();
            return true;
        }
        let app = self;
        let history_input = app.history_controls_event(&event, window.scale_factor());
        let _history_frame = crate::history_mode::frame::FrameworkInputFrame::new(
            &app.framework_input,
            history_input,
        );
        let detected_interaction = app
            .input
            .interaction
            .detected_from(&event, synthesized_touch);
        if let Some(environment) = detected_interaction {
            app.set_interaction_environment(environment);
            // The tree a widget builds takes up the way it is reached (a finger's target, a
            // hover's fade), so this event lands on the tree built for its modality: the frame
            // after the input would replace what the input itself opened.
            if app.build_tree_before_input() {
                app.lay_out_before_input(window);
            }
        }
        if history_input {
            // History's own chrome took this input: what widgets queued runs in no frame that
            // follows it, so the entry (or return) it asks for seals the live queue instead.
            app.input.passive_input = true;
            #[cfg(feature = "model")]
            {
                app.dispatch_framework_history_event(&event, window.scale_factor());
            }
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
        // never the shell — see the `pointer_space` field.
        #[cfg(feature = "model")]
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

        #[cfg(feature = "model")]
        let capture = app.begin_local_input_event(&event);
        let mut handled = false;
        let mut view_only_scroll = false;
        let mut history_handled = false;
        let mut focus_target = None;
        // A synthesized hover-clear may be handled by a control, but it is
        // presentation state and must not create an empty undo boundary.
        let mut is_undo_redo =
            synthesized_touch && matches!(event, WindowEvent::CursorMoved { .. });
        let mut floating_pointer_needs_redraw = false;

        // The palette owns the keyboard while open; Ctrl+K itself is
        // handled below, so let it through.
        #[cfg(feature = "model")]
        if app.palette_open() {
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
                    && tilo_ui_base::platform::HostShortcut::resolve(
                        app.input.platform,
                        c,
                        app.input.modifiers.state().shift_key(),
                    ) == Some(tilo_ui_base::platform::HostShortcut::Search)
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

        // A window asked for the thing a report is about takes the pointer and
        // the keys until the person points at it or leaves.
        #[cfg(feature = "model")]
        if !handled && let Some(pointing) = app.report_pointing.as_mut() {
            let focused = crate::shared_selection::subtree_selection(app.root_widget.as_ref());
            match pointing.event(
                &event,
                app.root_widget.as_ref(),
                f64::from(window.scale_factor()),
                focused,
            ) {
                report_point::Taken::Ignored => {}
                report_point::Taken::Redraw => {
                    // What is lit follows the pointer, which asks for a frame of
                    // its own: a pointer move alone asks for none.
                    handled = true;
                    window.request_redraw();
                }
                report_point::Taken::Left => {
                    app.report_pointing = None;
                    handled = true;
                }
                report_point::Taken::Picked(about, bounds) => {
                    let to = pointing.to().to_vec();
                    let carried = pointing.carried().to_vec();
                    app.report_pointing = None;
                    app.open_send_box(Some(about), bounds, None, &to, &carried);
                    handled = true;
                    window.request_redraw();
                }
            }
        }

        // The box the window stands with no menu around it has the pointer and
        // the keys before what the app shows, until it sends or goes.
        if !handled && app.window_box.is_open() {
            handled = app.window_box_event(&event, window.scale_factor());
            window.request_redraw();
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

        // Affects pointer click-through only; keyboard focus
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

        // A zoom gesture (Ctrl+Plus/Minus/0, Ctrl+scroll, a pinch) goes to a
        // widget whose content zooms by itself, a web page per site, before
        // anything else sees it.
        if !handled && app.offer_content_zoom(&event) {
            handled = true;
            window.request_redraw();
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
            // Find (and Find and Replace) opens the window bar's search,
            // which acts on this app in place, ahead of any find of the
            // app's own.
            if let Some(opening) = crate::widget::window_search::search_chord(
                &event,
                app.input.modifiers.state(),
                app.input.platform,
            ) && app.window_bar_takes(opening)
            {
                crate::window_toolbar::open_search(opening);
                handled = true;
            }
            match tilo_ui_base::platform::HostShortcut::resolve(app.input.platform, c, shift) {
                Some(tilo_ui_base::platform::HostShortcut::Undo) => {
                    app.perform_undo_request(tilo_ui_base::widget::undo_access::UndoRequest::Undo);
                    is_undo_redo = true;
                    handled = true;
                }
                Some(tilo_ui_base::platform::HostShortcut::Redo) => {
                    app.perform_undo_request(tilo_ui_base::widget::undo_access::UndoRequest::Redo);
                    is_undo_redo = true;
                    handled = true;
                }
                // Search of this app: the session's one search inside a
                // session, the window's own outside one, where a second
                // press closes it.
                #[cfg(feature = "model")]
                Some(tilo_ui_base::platform::HostShortcut::Search) => {
                    if app.palette_open() {
                        app.close_action_palette();
                    } else {
                        app.open_search(crate::widget::search_request::SearchRequest::this_app());
                    }
                    handled = true;
                }
                // Without Tilo's model a window has no search: the key reaches the app.
                #[cfg(not(feature = "model"))]
                Some(tilo_ui_base::platform::HostShortcut::Search) => {}
                Some(tilo_ui_base::platform::HostShortcut::History) => {
                    handled =
                        app.invoke_host_shortcut(tilo_ui_base::platform::HostShortcut::History);
                }
                Some(tilo_ui_base::platform::HostShortcut::Sidebar) => {
                    handled =
                        app.invoke_host_shortcut(tilo_ui_base::platform::HostShortcut::Sidebar);
                }
                None => {}
            }
            if handled {
                window.request_redraw();
            }
        }

        if !handled && app.handle_tab_navigation(&event, app.keyboard_nav_allowed(modal_rect)) {
            handled = true;
        }

        // A sideways wheel in a gesture the shell's window strip took is the
        // strip's to the end: what the shell does not scroll with pans the
        // strip the window stands in, and no view of this app sees any event
        // of it, so a gesture never splits between the app and the strip
        // ([`App::record_sideways_wheel`] decided at its first event).
        #[cfg(feature = "native")]
        if !handled
            && app.strip_takes_sideways_wheel(&event, window.scale_factor(), crate::time::now())
        {
            return true;
        }

        // Two-finger swipe, edge pull or mouse side button; delivered via
        // `on_nav_gesture`.
        if !handled && !synthesized_touch {
            use tilo_ui_base::gesture::GesturePhase;
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
                WindowEvent::MouseInput { .. } => {
                    tilo_ui_base::interaction::mouse_nav_direction(&event)
                }
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
                let consumed = app.surface_nav_gesture(dir);
                if consumed {
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
            let used_floating_pointer_space = floating_pointer_event.is_some();
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
                && (used_floating_pointer_space
                    || presentation_was_floating != presentation_is_floating);
            if was_mouse_click {
                tracing::debug!(
                    handled,
                    needs_redraw =
                        app.root_widget.needs_redraw() || app.root_widget.needs_animation_frame(),
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

        // A zoom gesture no widget took zooms the window's content.
        if !handled && app.zoom_unhandled(&event) {
            handled = true;
            window.request_redraw();
        }

        // What this window's content did with a sideways wheel: the gesture's
        // first event decides whether the app scrolls with it or the shell's
        // strip pans by it, and every later event of the gesture keeps that
        // owner ([`App::record_sideways_wheel`]).
        #[cfg(feature = "native")]
        app.record_sideways_wheel(&event, window.scale_factor(), handled, crate::time::now());

        // Arrows, Enter and Space, search and typed keys, the shell's tab
        // chords (unless the window answers them) and range steps nobody
        // consumed, unless the window holds them for what it waits for.
        if handled || !app.input.held.holding(app.root_widget.as_ref()) {
            let navigation = app.keyboard_nav_allowed(modal_rect);
            app.handle_unconsumed_key(&event, navigation, !window.hosts_tab_chords(), &mut handled);
        }

        // Input framework chrome took (a press on the window's own History) runs no control a
        // widget queued, and neither does a window still opening: the entry it asks for seals it,
        // and the frame that follows runs nothing either.
        #[cfg(feature = "model")]
        if !app.input.opening && !app.input.passive_input && app.dispatch_control_invocations() != 0
        {
            handled = true;
            app.needs_sync = true;
            app.layout.layout_dirty = true;
            window.request_redraw();
        }

        #[cfg(feature = "model")]
        {
            drop(capture);
            app.drain_local_input_capture();
        }

        #[cfg(feature = "model")]
        {
            if let Some(address) = app.root_widget.take_action_palette_request()
                && app.open_search_at_action(&address)
            {
                app.layout.layout_dirty = true;
                window.request_redraw();
            }

            if app.root_widget.take_search_request() && !app.palette_open() {
                app.open_search(crate::widget::search_request::SearchRequest::this_app());
                app.layout.layout_dirty = true;
                window.request_redraw();
            }
        }

        // A handled event (or one that moves focus) asks below for a sync and
        // a frame, and that sync can move what the tree shows: a pane the
        // click opened, an edit a view settles, a selection a list takes up.
        // Publishing now would build the whole tree for the press and again
        // after the sync; the frame that runs the sync publishes it once
        // (`App::publish_owed_action_surface`). A pointer move may ask for no
        // frame, and a view-only wheel for no sync, so both publish now.
        #[cfg(feature = "model")]
        let sync_follows = (handled || focus_target.is_some())
            && !(view_only_scroll && !history_handled && focus_target.is_none())
            && !matches!(event, WindowEvent::CursorMoved { .. });
        let publication = phases.start();
        #[cfg(feature = "model")]
        {
            app.broadcast_focus_presence();
            app.broadcast_location_presence();
            app.broadcast_composing_presence();
            app.broadcast_view_presence();
        }
        // A publication an earlier input owes is the frame's already. What a window publishes
        // (its actions, continuations and search index) is published into Tilo's model.
        #[cfg(feature = "model")]
        {
            if sync_follows || app.actions.action_surface_owed {
                app.actions.action_surface_owed = true;
            } else {
                app.publish_action_surface();
            }
            app.publish_continuation_metadata();
            app.publish_search_index();
        }
        phases.finish(InputPhase::Publication, publication);

        if let Some(id) = focus_target {
            app.focus_for_input(id, &event);
            handled = true;
        }

        let publication = phases.start();
        app.announce_state_change(state_pre);
        app.announce_text_review(review_pre);
        phases.finish(InputPhase::Publication, publication);

        if handled {
            let commit = phases.start();
            if view_only_scroll && !history_handled && focus_target.is_none() {
                app.commit_view_change();
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
            // redraw when nothing's dirty. Layout is another matter:
            // a widget says when its size moved, so a press that only
            // repaints (a button held, a row lit) is drawn, not laid out.
            let is_pointer_move = matches!(&event, WindowEvent::CursorMoved { .. });
            if app.root_widget.is_layout_dirty() || app.root_widget.layout_needs_measure() {
                app.layout.layout_dirty = true;
            }
            if !is_pointer_move
                || floating_pointer_needs_redraw
                || (app.root_widget.needs_redraw() || app.root_widget.needs_animation_frame())
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
            let target = app
                .tooltip_target_at(logical)
                .map(|(rect, label, _)| (rect, label));
            phases.finish(InputPhase::HitTest, hit_test);
            if app.input.current_cursor != desired {
                app.input.current_cursor = desired;
                window.set_cursor(desired);
            }
            // `on_pointer` keeps the dwell running over the same
            // region and restarts it on a new one.
            if app.input.tooltips.on_pointer(target, crate::time::now()) {
                window.request_redraw();
            }
        }

        if let (Some(start), Some(p)) = (__prof_input, app.profiler.as_mut()) {
            p.record_input(start, start.elapsed(), phases.phases());
        }
        handled
    }
    /// The system's back (or forward) gesture, once its host recognised one (a two-finger swipe,
    /// an edge pull, a mouse side button, the back key), delivered to the whole tree. Whether
    /// something took it; what took it syncs in the next frame.
    pub fn surface_nav_gesture(&mut self, dir: tilo_ui_base::gesture::NavDirection) -> bool {
        let _entry = self.enter_wake_scope();
        let taken = self
            .root_widget
            .on_nav_gesture(dir, &self.state.doc, &self.state.root);
        if taken {
            self.needs_sync = true;
            self.mark_semantic_tree_stale();
        }
        taken
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

    /// Close every modal this window presents, so it leaves as its own way
    /// out takes it down: what a host whose surface is a modal sheet runs
    /// when the shell closes the sheet, so nothing stays up drawn taking no
    /// input. Whether any was presented.
    pub fn dismiss_presented_modal(&mut self) -> bool {
        let dismissed = self.root_widget.dismiss_presented_modal();
        if dismissed {
            self.needs_sync = true;
        }
        dismissed
    }

    /// Whether the window still presents a modal, arriving, open or leaving:
    /// a host that stands a modal sheet redraws until this is false.
    pub fn presents_modal(&self) -> bool {
        self.root_widget.presents_modal()
    }

    /// Whether the app's model woke the window for a frame it has not run
    /// yet: background work that ended, or a read that goes on frame by
    /// frame. A window that rests has none.
    pub fn wake_pending(&self) -> bool {
        self.wake_scope
            .as_ref()
            .is_some_and(crate::wake::WakeScope::is_pending)
    }

    /// Which surface this app is on the thread's motion clock: the frames it
    /// asks for and draws are its own, so a shell drawing on the same thread
    /// never meets a lock screen's wish for it. Its state box stays put for
    /// as long as the app does.
    pub(crate) fn motion_owner(&self) -> usize {
        std::ptr::from_ref(&*self.state) as usize
    }

    /// When the app next wants a frame with no input to bring one, for a
    /// host that schedules its own frames and asks at `now`: `now` itself
    /// while something moves, else the earliest deadline its widgets set (a
    /// caret's blink, the clock's next minute) or a tooltip's dwell runs out.
    pub fn surface_next_frame(&self, now: crate::time::Instant) -> Option<crate::time::Instant> {
        tilo_ui_base::motion::own(self.motion_owner(), || self.surface_next_frame_own(now))
    }

    /// [`Self::surface_next_frame`] with whether something moves (`moving`, [`Self::surface_moving`])
    /// already found: a host that asks both for one schedule shares the motion walk rather than
    /// asking the tree twice.
    pub(crate) fn surface_next_frame_moving(
        &self,
        now: crate::time::Instant,
        moving: bool,
    ) -> Option<crate::time::Instant> {
        tilo_ui_base::motion::own(self.motion_owner(), || {
            self.surface_next_frame_own_moving(now, moving)
        })
    }

    /// Whether something in the app moves at `now`: a widget's motion, a
    /// frame a motion asked for, or a press or hover still settling. A host
    /// draws it again while it does, as a window of its own is redrawn.
    pub fn surface_moving(&self, now: crate::time::Instant) -> bool {
        tilo_ui_base::motion::own(self.motion_owner(), || self.surface_moving_own(now))
    }

    /// [`Self::surface_moving`], asked as this app.
    fn surface_moving_own(&self, now: crate::time::Instant) -> bool {
        self.root_widget.needs_animation_frame()
            || tilo_ui_base::motion::frames_wanted(now)
            || self
                .presence_ctx
                .interaction_animation_demand(now, &self.theme)
    }

    /// [`Self::surface_next_frame`], asked as this app.
    fn surface_next_frame_own(&self, now: crate::time::Instant) -> Option<crate::time::Instant> {
        self.surface_next_frame_own_moving(now, self.surface_moving_own(now))
    }

    /// [`Self::surface_next_frame_own`] with whether the app moves already found: what moves is due
    /// now, so nothing walks the tree for a deadline it would not use.
    fn surface_next_frame_own_moving(
        &self,
        now: crate::time::Instant,
        moving: bool,
    ) -> Option<crate::time::Instant> {
        // Due at the very instant the host asks at: read from its own clock instead, the answer
        // could fall after the host's `now` on a slow frame, and a motion's last frame would
        // never come due.
        if moving {
            return Some(now);
        }
        [
            self.root_widget.next_wake_deadline(),
            self.input.tooltips.reveal_at(),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// What a window of its own does for its accessibility before each
    /// frame: apply the preference a control asked for (larger text, high
    /// contrast, the screen reader) and say what changed in live regions.
    /// Returns whether a preference changed, which repaints the frame.
    pub fn surface_accessibility(&mut self) -> bool {
        let _entry = self.enter_wake_scope();
        let changed = self.apply_accessibility_request();
        self.scan_live_regions();
        changed
    }

    /// Hand `payload` to the app's root, as an external event a window of
    /// its own receives ([`tilo_ui_base::widget::Widget::on_external_event`]).
    pub fn external_event(&mut self, payload: &str) {
        let _entry = self.enter_wake_scope();
        self.root_widget.on_external_event(
            payload,
            &mut tilo_ui_base::widget::Edits::lend(&mut self.state.doc),
            &self.state.root,
        );
        self.needs_sync = true;
    }

    /// A finger on a surface whose host reads the touchscreen itself: the
    /// pointer events a tap, drag or pinch makes go to the widgets as the
    /// platform window's do (`emulator` keeps the gesture across contacts),
    /// then the contact itself for widgets that read fingers. Once every
    /// gesture has landed, touch is the input in use. While the built-in
    /// screen reader is on, a finger explores instead and no widget sees it
    /// ([`Self::explore_by_touch`]).
    pub fn surface_touch(
        &mut self,
        window: &dyn AppSurface,
        touch: winit::event::Touch,
        emulator: &mut tilo_ui_base::interaction::TouchPointerEmulator,
    ) -> bool {
        let scale = window.scale_factor();
        if self.explore_by_touch(&touch, scale) {
            window.request_redraw();
            return true;
        }
        let events = emulator.events_claimed(&touch, |at| {
            self.touch_claim_at(Point::new(at.x / scale, at.y / scale))
        });
        let mut handled = false;
        for event in events {
            handled |= self.surface_input(window, event.window_event(touch.device_id), true);
        }
        if emulator.settled() {
            self.set_interaction_environment(self.input.interaction.with_touch_modality());
        }
        handled | self.surface_input(window, WindowEvent::Touch(touch), false)
    }

    /// A still finger that reached the long-press delay by `now`: the
    /// secondary click it makes, though no contact marks the moment.
    pub fn surface_touch_hold(
        &mut self,
        window: &dyn AppSurface,
        emulator: &mut tilo_ui_base::interaction::TouchPointerEmulator,
        now: crate::time::Instant,
    ) -> bool {
        let Some((device_id, events)) = emulator.poll(now) else {
            return false;
        };
        let mut handled = false;
        for event in events {
            handled |= self.surface_input(window, event.window_event(device_id), true);
        }
        if emulator.settled() {
            self.set_interaction_environment(self.input.interaction.with_touch_modality());
        }
        handled
    }

    /// The modifier keys held, as the surface reports them.
    pub fn set_surface_modifiers(&mut self, modifiers: winit::event::Modifiers) {
        self.input.modifiers = modifiers;
    }

    /// Build the app's tree from its model without laying it out or painting
    /// it: for a host that hands the app focus or input before its first
    /// frame, or between a change of the app's structure and the frame that
    /// shows it, so both find the app's controls. Returns whether a tree was
    /// built; the next frame then repaints whole, as `damage` cannot say what
    /// the tree it replaced painted.
    pub fn surface_build_tree(&mut self, damage: &mut SurfaceDamage) -> bool {
        let _entry = self.enter_wake_scope();
        self.sync_model(false);
        if self.layout.root_node.is_none() {
            self.init_layout();
        } else if !self.build_tree_before_input() {
            return false;
        }
        damage.repaint("a tree built between frames");
        true
    }

    /// Rebuild the app's tree when its structure changed since it was last
    /// built, so input or focus handed to it between frames finds the
    /// controls it shows now. Whether it rebuilt.
    fn build_tree_before_input(&mut self) -> bool {
        if self.layout.root_node.is_none() {
            // Input can arrive before the app's first frame: a sheet opened
            // and typed into in one step, a panel's first press. Build and
            // lay out the tree it belongs to now, with the keyboard where
            // the app puts it, so the input lands instead of dropping.
            self.init_layout();
            self.rebuild_structure();
            if self.autofocus_first
                && self.focused_node.is_none()
                && let Some(id) = self.initial_live_focus_target()
            {
                self.set_focused_node(Some(id));
            }
            return true;
        }
        let changed = self.take_root_structure_dirty();
        self.layout.structure_dirty |= changed;
        if !self.layout.structure_dirty {
            return false;
        }
        self.rebuild_structure();
        true
    }

    /// Lay out `window`'s tree now when it was rebuilt since the last frame, as the frame that
    /// follows would: a rebuilt tree has no places, and input dispatched against it reaches no
    /// control at all — a press the person made on what they see ran nothing, and nothing said
    /// why. The frame draws what this pass laid out, as it would its own.
    fn lay_out_before_input(&mut self, window: &dyn AppSurface) {
        let Some(root) = self.layout.root_node else {
            return;
        };
        let scale = window.scale_factor();
        let size = window.inner_size();
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a window's logical size is a small non-negative pixel count"
        )]
        let (width, height) = (
            (f64::from(size.width) / scale.max(f64::EPSILON)) as u32,
            (f64::from(size.height) / scale.max(f64::EPSILON)) as u32,
        );
        #[allow(
            clippy::cast_possible_truncation,
            reason = "a window's scale is a small ratio"
        )]
        let scale_factor = scale as f32;
        if crate::app::run_layout_pass(self, root, width, height, scale_factor, true, None)
            .is_none()
        {
            tracing::warn!("a rebuilt tree could not be laid out for the input that follows it");
        }
        self.layout.layout_dirty = true;
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
        // invocations and idle model work.
        // What the frame costs and draws, for a profiler the app runs with.
        let started = self.profiler.as_ref().map(|_| crate::time::now());
        let commands_before = scene.encoding().draw_tags.len();
        #[cfg(test)]
        let pump_probe = crate::frame_probe::span("frame.pump");
        let pump = self.pump_hosted_model_report();
        #[cfg(test)]
        drop(pump_probe);
        if let Some(profiler) = self.profiler.as_mut() {
            profiler.record_since(crate::profiling::Pass::Sync, started);
        }
        // The model's pass took in what landed from elsewhere; the frame
        // builds, lays out and draws that state. Work landing meanwhile is
        // the next frame's, which its wake asks for.
        #[allow(
            unused_variables,
            reason = "a build without the window's own runner lays out in hosted frames only, and this guard is what the hosted frame's passes read"
        )]
        let laying_out = tilo_ui_base::motion::laying_out_frame();
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
        #[cfg(test)]
        let structure_probe = crate::frame_probe::span("frame.structure");
        let changed = self.take_root_structure_dirty();
        #[cfg(test)]
        drop(structure_probe);
        if replaced {
            self.layout.request_rebuild();
        }
        self.layout.structure_dirty |= changed;
        let structure_rebuilt = self.layout.structure_dirty;
        // Every widget and its box as the last frame left them, before
        // anything is rebuilt: compared by identity only, never reached.
        #[cfg(test)]
        let boxes_probe = crate::frame_probe::span("frame.boxes");
        let boxes_before = widget_boxes(self.root_widget.as_ref());
        #[cfg(test)]
        drop(boxes_probe);
        if structure_rebuilt {
            self.rebuild_structure();
        }
        let Some(root_node) = self.layout.root_node else {
            damage.repaint("no layout");
            return true;
        };
        let rebuilds = (self.layout.rebuilds, self.layout.remounts);
        // A tree built between this frame and the last one (a state tick's, a
        // sync's) is painted new where no widget's damage reaches.
        let rebuilt_since_frame = self.rebuilt_since_frame();
        #[cfg(test)]
        let layout_probe = crate::frame_probe::span("frame.layout");
        let laid_out = run_layout_pass(
            self,
            root_node,
            size.width,
            size.height,
            scale_factor,
            structure_rebuilt,
            None,
        );
        #[cfg(test)]
        drop(layout_probe);
        let Some((geometry_changed, _, _)) = laid_out else {
            damage.repaint("no layout");
            return true;
        };
        // The tree this frame built before its layout pass is the frame's rebuild too.
        if structure_rebuilt {
            self.layout.last_pass.rebuilt = true;
            self.layout.last_pass.remounted = false;
        }
        // A frame whose model pass took up nothing that arrived, synchronized nothing, published
        // nothing and changed no structure: what a window at rest draws, and what a caller-asked
        // frame at rest stays. What a widget has yet to paint is this frame's own drawing, not
        // more work: the frame that ends a mark paints its last pose, and asks for nothing after.
        let quiet = !pump.updated
            && !pump.synced
            && !pump.published
            && !pump.presentation_changed
            && !structure_rebuilt
            && !self.root_widget.needs_idle_sync()
            && !self.root_widget.needs_animation_frame()
            && !self.wake_pending()
            && !self.actions.bound_calls_running()
            && !tilo_ui_base::widget::controls::has_pending_control_invocation(
                self.root_widget.as_ref(),
            );
        let frames = &mut self.layout.frames;
        frames.laid_out = frames.laid_out.saturating_add(1);
        if quiet {
            frames.quiet = frames.quiet.saturating_add(1);
        }
        if self.layout.last_pass.rebuilt {
            frames.rebuilt = frames.rebuilt.saturating_add(1);
            let built = u64::try_from(self.layout.node_map.len()).unwrap_or(u64::MAX);
            frames.nodes_built = frames.nodes_built.saturating_add(built);
        }
        if geometry_changed {
            self.input.pointer_replay.mark_unsettled();
        }
        #[cfg(feature = "model")]
        let search_pending = self
            .actions
            .action_palette
            .as_deref_mut()
            .is_some_and(|palette| palette.advance_search_page().pending);
        #[cfg(not(feature = "model"))]
        let search_pending = false;
        // What a window of its own repaints whole: a rebuilt tree, moved
        // geometry, another record, and what paints over the widgets.
        // Carets, selections and marks widgets paint from presence carry no
        // widget damage (`draw_presence_layer`), so a change to them damages
        // where the last frame painted presence and where this one does —
        // unless the change is one no marks bound (a peer joining a widget,
        // a name, semantic activity), which still repaints the window.
        let presence =
            damage.widgets_presence_changed(self.presence_ctx.widget_presence(crate::time::now()));
        let hud = self
            .profiler
            .as_ref()
            .is_some_and(crate::profiling::FrameProfiler::hud_enabled);
        // A relayout that moved some of the same widgets repaints where each
        // was and is: what else is there is drawn by widgets that did not
        // move, and their own damage covers any change to them.
        // A layout pass that recomposed the tree may have replaced widgets.
        let recomposed = (self.layout.rebuilds, self.layout.remounts) != rebuilds;
        let described = (structure_rebuilt || recomposed) && self.layout.frame_rebuilds_described;
        let relaid = !described && !recomposed && geometry_changed && !structure_rebuilt;
        #[cfg(test)]
        let boxes_probe = crate::frame_probe::span("frame.boxes");
        let boxes_after = (described || relaid).then(|| widget_boxes(self.root_widget.as_ref()));
        #[cfg(test)]
        drop(boxes_probe);
        let moved = match &boxes_after {
            Some(after) => Some(changed_boxes(&boxes_before, after)),
            None if recomposed => None,
            None => Some(Vec::new()),
        };
        // A theme made again since the last frame paints every pixel of the
        // tree in new colours, and no widget's damage describes that.
        let themed = self.theme_changed();
        let full = [
            (replaced, "another component"),
            (structure_rebuilt && !described, "a rebuilt tree"),
            (rebuilt_since_frame && !structure_rebuilt, "a rebuilt tree"),
            (themed, "a new theme"),
            (moved.is_none(), "moved geometry"),
            (self.debug_mode, "debug outlines"),
            (self.palette_open(), "the action palette"),
            (hud, "the profiler HUD"),
            (
                // A host that draws only this frame's damage leaves marks it
                // does not draw unpainted, and their notes with them: the
                // frame cannot name where the last one painted them either.
                presence.unbounded(damage.culls),
                "presence marks",
            ),
        ]
        .into_iter()
        .find_map(|(repaint, why)| repaint.then_some(why));
        // A host that repaints only this frame's damage from its scene is
        // drawn only what that damage needs (`tilo_ui_base::draw_cull`), unless the
        // frame repaints whole or paints what its widgets' damage does not
        // describe.
        let culls = damage.culls
            && full.is_none()
            && !self.presents_history()
            && !tilo_ui_base::scene_parts::is_tracked(scene);
        let mut culling = culls.then(|| std::mem::take(&mut self.layout.cull));
        let cull_frame = culling
            .as_mut()
            .map(tilo_ui_base::draw_cull::Culling::begin);
        #[cfg(test)]
        let draw_probe = crate::frame_probe::span("frame.draw");
        let drawn = draw_frame_content_with(
            self,
            scene,
            (size.width, size.height),
            scale_factor,
            None,
            culling.as_mut().map(|culling| &mut culling.scenes),
        );
        #[cfg(test)]
        drop(draw_probe);
        drop(laying_out);
        // The surface a handled input left for this frame, now that its sync
        // ran and the frame built the tree, as a window of its own pays it.
        #[cfg(feature = "model")]
        {
            #[cfg(test)]
            let publish_probe = crate::frame_probe::span("frame.publish");
            self.publish_owed_action_surface();
            #[cfg(test)]
            drop(publish_probe);
        }
        // Members' marks over the widgets, for a host that shows the
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
            // Over the widgets: a culled frame composes them first.
            if culling.is_none() {
                scene.append(peers.layer.scene(), None);
            }
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
                } else {
                    Some(changed_boxes(&before, &after))
                }
            }
            (moved, _) => moved,
        };
        // What moved besides what scrolls copy, and what came or went in a
        // scrolled content; `None` when a rebuild no widget described moved
        // what cannot be said.
        let moved_besides = |offers: &[tilo_ui_base::scroll_copy::ScrollOffer]| match &boxes_after {
            Some(after) if described || relaid => {
                let moves = tilo_ui_base::scroll_copy::moves_besides(
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
        // Where this frame painted what presence places over and inside the
        // widgets, in scene pixels: the damage a change to those marks names,
        // with the last frame's (`SurfaceDamage::presence_paint`).
        let presence = presence.with_paint(self.presence_ctx.presence_paint());
        let plan_scale = (scale, damage.copy_scale);
        let plan_size = (
            device_extent(size.width, damage.copy_scale),
            device_extent(size.height, damage.copy_scale),
        );
        #[cfg(test)]
        let damage_probe = crate::frame_probe::span("frame.damage");
        let copies_before = damage.copies.len();
        damage.frame(full, &drawn, scale, peers.as_ref(), presence, |regions| {
            let root = self.root_widget.as_ref();
            if !copying {
                collect_submitted_widget_damage(root, regions);
                regions.extend(moved.into_iter().flatten());
                return tilo_ui_base::scroll_copy::CopyPlan::default();
            }
            let offers = tilo_ui_base::scroll_copy::accepting(|| {
                collect_submitted_widget_damage(root, regions)
            });
            if offers.is_empty() {
                regions.extend(moved.into_iter().flatten());
                return tilo_ui_base::scroll_copy::CopyPlan::default();
            }
            // What is painted over the widgets (a menu, the palette, a
            // tooltip, peer marks that cannot be bounded) does not move with
            // a scroll: the scrollers repaint.
            if drawn.painted_over || marks_unbounded {
                regions.extend(moved.into_iter().flatten());
                regions.extend(
                    offers
                        .iter()
                        .map(tilo_ui_base::scroll_copy::ScrollOffer::viewport),
                );
                return tilo_ui_base::scroll_copy::CopyPlan::default();
            }
            let Some(besides) = moved_besides(&offers) else {
                regions.extend(moved.into_iter().flatten());
                regions.extend(
                    offers
                        .iter()
                        .map(tilo_ui_base::scroll_copy::ScrollOffer::viewport),
                );
                return tilo_ui_base::scroll_copy::CopyPlan::default();
            };
            regions.extend(besides);
            let history = self.history_widget();
            tilo_ui_base::scroll_copy::plan_scaled(
                &offers,
                &std::iter::once(root).chain(history).collect::<Vec<_>>(),
                plan_scale,
                plan_size,
            )
        });
        #[cfg(test)]
        drop(damage_probe);
        let copied = damage.copies.len() != copies_before;
        if let Some(mut culling) = culling {
            let left_out = cull_frame
                .as_ref()
                .is_some_and(tilo_ui_base::draw_cull::Frame::skipped_any);
            let holds = cull_frame
                .as_ref()
                .is_none_or(|frame| frame.holds(damage.pending.as_deref()));
            drop(cull_frame);
            if !holds {
                #[cfg(test)]
                let _redraw_probe = crate::frame_probe::span("frame.redraw");
                redraw_culled_widgets(self, &mut culling.scenes, &drawn);
            }
            culling.tally(left_out, !holds);
            let secret = culling.shows_secret(self.floating.secret_on_screen, left_out && holds);
            self.set_secret_on_screen(secret);
            compose_culled(scene, &culling.scenes);
            if let Some(peers) = &peer_layer {
                scene.append(peers.layer.scene(), None);
            }
            culling.repainted(damage.pending.as_deref().unwrap_or_default(), copied);
            self.layout.cull = culling;
        } else if damage.culls {
            self.layout
                .cull
                .repainted(damage.pending.as_deref().unwrap_or_default(), copied);
        }
        if let Some(mut peers) = peer_layer {
            peers.layer.presented();
            damage.peers = Some(peers);
        }
        #[cfg(test)]
        let complete_probe = crate::frame_probe::span("frame.complete");
        complete_native_frame_content(self, |_| {});
        #[cfg(test)]
        drop(complete_probe);
        let commands = u64::try_from(
            scene
                .encoding()
                .draw_tags
                .len()
                .saturating_sub(commands_before),
        )
        .unwrap_or(u64::MAX);
        self.layout.frames.drawn_commands =
            self.layout.frames.drawn_commands.saturating_add(commands);
        if let (Some(profiler), Some(started)) = (self.profiler.as_mut(), started) {
            profiler.record_draw_commands(commands);
            profiler.commit_frame(u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX));
        }
        // The frame laid out the content keys typed before it arrived waited for: they go to it,
        // and the next frame shows what they did.
        let released = self.release_held_input(surface);
        // The frame that laid the window out is the opening: from the next frame on, what a
        // widget queued runs as a frame's own work.
        self.input.opening = false;
        released
            || search_pending
            || self.needs_sync
            || self.layout.layout_dirty
            // The palette arrives outside the app's root too: a host that
            // repaints the app only when it asks (an embedded app) would
            // otherwise keep its first, nearly transparent pose on screen.
            || self.palette_moving()
            // History's panel moves outside the app's root (it slides in).
            || self
                .history_widget()
                .is_some_and(|history| history.needs_animation_frame())
            || self
                .root_widget
                .next_wake_deadline()
                .is_some_and(|deadline| deadline <= crate::time::now())
    }
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

/// The members' layer a surface paints over the widgets as a window
/// of its own does ([`SurfaceDamage::painting_peers`]).
#[derive(Default)]
struct PeerLayer {
    layer: tilo_ui_base::presence::PresenceLayer,
    bounds: tilo_ui_base::scene_parts::DrawBounds,
}

/// What the members' layer did in one frame: its damage against the
/// frame before (`None` when it cannot be proven), and where it paints, on
/// screen and now (`None` when that cannot be bounded), in scene pixels.
struct PeerFrame {
    changed: Option<Vec<KurboRect>>,
    marks: Option<tilo_ui_base::presence::LayerMarks>,
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
    tooltip: Option<KurboRect>,
    widgets: Option<tilo_ui_base::presence::WidgetPresence>,
    /// Why the last frame repainted the surface whole, if it did.
    repainted: Option<&'static str>,
    /// Whether the host copies what a scroll keeps on screen
    /// ([`Self::copying`]).
    copying: bool,
    /// The copies of the frames since the last take, in device pixels at
    /// [`Self::copy_scale`] per scene pixel, to make before repainting the
    /// damage.
    copies: Vec<tilo_ui_base::scroll_copy::DeviceCopy>,
    /// How many pixels of the host's target one pixel of the surface's
    /// scene is ([`Self::set_copy_scale`]).
    copy_scale: f64,
    /// The members' layer, when the surface paints it.
    peers: Option<Box<PeerLayer>>,
    /// Whether the host repaints only each frame's damage from that
    /// frame's scene ([`Self::drawing_only_damage`]).
    culls: bool,
    /// Where the last frame painted what presence places over and inside the
    /// widgets, in scene pixels, and whether that frame's marks are any: the
    /// old carets and chips a frame whose marks moved repaints
    /// ([`PresenceChange::with_paint`]).
    presence_painted: Vec<KurboRect>,
}

/// How what widgets paint from presence changed since the last frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum PresenceChange {
    /// Nothing changed.
    #[default]
    None,
    /// Only the marks widgets paint from their own content moved: where the
    /// last frame painted them and where this one does is the damage.
    Marks,
    /// A change no mark bounds (a peer on a widget, a name, semantic
    /// activity): the surface repaints.
    Unbounded,
}

impl PresenceChange {
    /// Whether the surface must repaint rather than damage the marks: the
    /// change is one no mark bounds, or the host draws only this frame's
    /// damage, so marks its damage leaves out are neither drawn nor noted.
    const fn unbounded(self, culls: bool) -> bool {
        matches!(self, Self::Unbounded) || (culls && matches!(self, Self::Marks))
    }

    /// This frame's paint of those marks, in scene pixels.
    fn with_paint(self, painted: Vec<KurboRect>) -> PresencePaint {
        PresencePaint {
            change: self,
            painted,
        }
    }
}

/// One frame's presence change and where it painted the marks, in scene
/// pixels.
struct PresencePaint {
    change: PresenceChange,
    painted: Vec<KurboRect>,
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
            culls: false,
            presence_painted: Vec::new(),
        }
    }
}

impl SurfaceDamage {
    /// For a host that presents each frame over the pixels of the last one
    /// and makes the copies [`Self::take_copying`] returns before it repaints
    /// the damage: a scroll then copies what stays on screen instead of
    /// damaging its viewport ([`tilo_ui_base::scroll_copy`]).
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

    /// Takes after every frame, for a host whose target keeps the pixels it presented: the
    /// frame draws only what its damage needs, and the host repaints what [`Self::take`]
    /// returns of that frame's scene alone — never more of it, and never the scene again
    /// later. A scroll then draws the strip it exposed and what changed, not every row in view
    /// (`tilo_ui_base::draw_cull`); a host that copies too plans its copies as well
    /// ([`Self::copying`]).
    #[must_use]
    pub fn drawing_only_damage(mut self) -> Self {
        self.culls = true;
        self
    }

    /// Draw only the damage each frame brings, or every widget again: what a host asks for a
    /// frame whose target is new or whole (a resized surface, another target entirely).
    pub fn set_drawing_only_damage(&mut self, only: bool) {
        self.culls = only;
    }

    /// Whether a frame repainted the surface whole for `why`, as the host's target is new: the
    /// damage taken since describes nothing of it.
    pub fn repaint_whole(&mut self, why: &'static str) {
        self.repaint(why);
    }

    /// Paint members' pointers, names and activity over the widgets,
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
    /// surface's scene (an app drawn into a shell's logical scene shown at
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
    ) -> Option<(Vec<tilo_ui_base::scroll_copy::DeviceCopy>, Vec<KurboRect>)> {
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

    /// How what widgets paint from presence changed: nothing, marks that a
    /// frame's presence paint bounds (a caret or selection moving, a typing
    /// mark fading), or a change no mark bounds (a peer on a widget, a name,
    /// semantic activity), which repaints the surface.
    fn widgets_presence_changed(
        &mut self,
        widgets: tilo_ui_base::presence::WidgetPresence,
    ) -> PresenceChange {
        let change = match &self.widgets {
            // Nothing on screen to compare a first frame with.
            None => PresenceChange::Unbounded,
            Some(previous) => {
                if previous == &widgets {
                    PresenceChange::None
                } else if widgets.differs_only_in_marks(previous) {
                    PresenceChange::Marks
                } else {
                    PresenceChange::Unbounded
                }
            }
        };
        self.widgets = Some(widgets);
        change
    }

    /// Add one drawn frame: repainted whole for the reason `full` gives,
    /// when a layer is painted over the widgets or was last frame, and
    /// otherwise the widget damage `collect` reports (logical, scaled by
    /// `scale` to the scene) with the tooltip where it was and where it is,
    /// where the last frame and this one painted presence (`presence`, in
    /// the scene), and the copies and damage of the scrolls it planned.
    fn frame(
        &mut self,
        full: Option<&'static str>,
        drawn: &DrawnFrame,
        scale: f64,
        peers: Option<&PeerFrame>,
        presence: PresencePaint,
        collect: impl FnOnce(&mut Vec<KurboRect>) -> tilo_ui_base::scroll_copy::CopyPlan,
    ) {
        let layer_shown = std::mem::replace(&mut self.top_layer_drawn, drawn.top_layer.is_some());
        let tooltip_shown = std::mem::replace(&mut self.tooltip, drawn.tooltip);
        let PresencePaint { change, painted } = presence;
        let painted = std::mem::replace(&mut self.presence_painted, painted);
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
        let content_changed = !regions.is_empty();
        // Planned, carried and kept in the host's device pixels; pending in
        // the scene's.
        let unit = self.copy_scale;
        let to_device = Affine::scale(unit);
        let mut changed: Vec<KurboRect> = regions
            .into_iter()
            .map(|rect| Affine::scale(scale * unit).transform_rect_bbox(rect))
            .collect();
        // Carets, selection bands, typing washes and the chips that name
        // them are painted from the widgets' content and from presence: a
        // frame whose marks moved, or that changed content they stand on,
        // repaints where the last frame painted presence and where this one
        // does, and nothing else.
        if change == PresenceChange::Marks
            || (content_changed && !(painted.is_empty() && self.presence_painted.is_empty()))
        {
            changed.extend(
                painted
                    .iter()
                    .chain(self.presence_painted.iter())
                    .map(|rect| {
                        to_device
                            .transform_rect_bbox(rect.expand())
                            .inflate(1.0, 1.0)
                    }),
            );
        }
        // The tooltip paints over the widgets: a label that changes in
        // place has no other damage. Nor do the members' marks.
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
            tilo_ui_base::scroll_copy::carry(&planned.copies, &mut changed);
            if let Some(marks) = peers.and_then(|peers| peers.marks.as_ref()) {
                let device = |rects: &[KurboRect]| -> Vec<KurboRect> {
                    rects
                        .iter()
                        .map(|rect| to_device.transform_rect_bbox(*rect))
                        .collect()
                };
                tilo_ui_base::scroll_copy::marks_over_copies(
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
                    .map(tilo_ui_base::scroll_copy::DeviceCopy::destination_rect),
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
#[cfg(feature = "model")]
mod tests {
    use super::*;
    use crate::testing::Dispatch;
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;
    use tilo_ui_base::widget::TaffyTreeExt;

    thread_local! {
        static CLICKS: RefCell<Option<Rc<RefCell<Vec<Point>>>>> = const { RefCell::new(None) };
        static PHYSICAL: RefCell<Vec<tilo_ui_base::widget::PhysicalInput>> = const { RefCell::new(Vec::new()) };

    }

    include!("native_publication_scroll_tests.rs");

    tilo_test_env::scenario! {
        fn a_changed_layout_repaints_only_the_boxes_that_changed;

        phase a_relayout_repaints_where_moved_widgets_were_and_are;
        {
            let (a, b) = (1_u8, 2_u8);
            let (a, b) = (
                (std::ptr::from_ref(&a).cast::<()>(), 1),
                (std::ptr::from_ref(&b).cast::<()>(), 1),
            );
            let field = KurboRect::new(0.0, 0.0, 100.0, 20.0);
            let label = KurboRect::new(0.0, 30.0, 50.0, 40.0);
            let grown = KurboRect::new(0.0, 0.0, 140.0, 20.0);
            assert_eq!(
                changed_boxes(&[(a, field), (b, label)], &[(a, grown), (b, label)]),
                vec![field, grown]
            );
            assert_eq!(
                changed_boxes(&[(a, field), (b, label)], &[(a, field), (b, label)]),
                Vec::<KurboRect>::new()
            );
            // Two widgets that swapped places repaint where each was and is.
            assert_eq!(
                changed_boxes(&[(a, field), (b, label)], &[(b, field), (a, label)]),
                vec![label, field, field, label]
            );
            // One that came into view repaints where it is.
            assert_eq!(
                changed_boxes(&[(a, field)], &[(a, field), (b, label)]),
                vec![label]
            );
        }

        phase a_described_rebuild_repaints_the_widgets_that_came_went_or_moved;
        {
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
    }

    tilo_test_env::scenario! {
        fn presence_marks_damage_where_they_were_and_are;

        phase a_caret_moving_is_a_marks_change_and_a_peer_is_not;
        {
            let mut damage = SurfaceDamage::default();
            let mut presence = tilo_ui_base::presence::PresenceCtx::default();
            let key = tilo_ui_base::presence::PresenceKey::widget(&[
                tilo_ui_base::presence::PathSegment::Field("title".into()),
            ])
            .expect("a widget key");
            let member = tilo_ui_base::permissions::MemberId::person("ada");
            let now = crate::time::now();
            presence.set_caret(&key, member.clone(), 3);
            // Nothing on screen to compare the first frame with.
            assert_eq!(
                damage.widgets_presence_changed(presence.widget_presence(now)),
                PresenceChange::Unbounded
            );
            // The same presence again is nothing to draw.
            assert_eq!(
                damage.widgets_presence_changed(presence.widget_presence(now)),
                PresenceChange::None
            );
            // A caret moving is only the marks, wherever it moved to.
            presence.set_caret(&key, member.clone(), 9);
            assert_eq!(
                damage.widgets_presence_changed(presence.widget_presence(now)),
                PresenceChange::Marks
            );
            assert!(!PresenceChange::Marks.unbounded(false));
            assert!(PresenceChange::Marks.unbounded(true));
            // A peer standing on a widget is painted from presence no mark
            // bounds: the surface repaints.
            presence.here.insert(key, vec![member]);
            assert_eq!(
                damage.widgets_presence_changed(presence.widget_presence(now)),
                PresenceChange::Unbounded
            );
        }

        phase a_frame_repaints_where_it_painted_presence_and_where_the_last_one_did;
        {
            let window = KurboRect::new(0.0, 0.0, 640.0, 400.0);
            let drawn = DrawnFrame {
                transform: Affine::IDENTITY,
                window_rect: window,
                draw_us: 0,
                top_layer: None,
                tooltip: None,
                painted_over: false,
            };
            // Where a caret and its chip stood, and where they stand now.
            let was = KurboRect::new(40.0, 40.0, 41.0, 56.0);
            let is = KurboRect::new(88.0, 40.0, 89.0, 56.0);
            let covers = |damage: &mut SurfaceDamage, mark: KurboRect| {
                let taken = damage.take().expect("the frame could be described");
                assert!(
                    taken.iter().any(|rect| rect.contains(mark.center())),
                    "{mark:?} is not repainted: {taken:?}"
                );
                let other = if mark == was { is } else { was };
                assert!(
                    !taken.iter().any(|rect| {
                        rect.contains(KurboRect::new(other.x0, 0.0, other.x0, 1.0).center())
                    }),
                    "{other:?} is repainted before it was painted: {taken:?}"
                );
            };
            let mut damage = SurfaceDamage::default();
            let _ = damage.take();
            // The frame that first paints the caret repaints it.
            let first = PresenceChange::Marks.with_paint(vec![was]);
            damage.frame(None, &drawn, 1.0, None, first, |_| {
                tilo_ui_base::scroll_copy::CopyPlan::default()
            });
            covers(&mut damage, was);
            // The caret moved: where it was and where it is, and no further.
            let moved = PresenceChange::Marks.with_paint(vec![is]);
            damage.frame(None, &drawn, 1.0, None, moved, |_| {
                tilo_ui_base::scroll_copy::CopyPlan::default()
            });
            let taken = damage.take().expect("the frame could be described");
            for mark in [was, is] {
                assert!(
                    taken.iter().any(|rect| rect.contains(mark.center())),
                    "{mark:?} is not repainted: {taken:?}"
                );
            }
            assert!(
                taken.iter().all(|rect| rect.width() < window.width() / 4.0),
                "a caret repainted half the window: {taken:?}"
            );
        }

        phase content_changing_under_marks_repaints_the_marks_too;
        {
            let window = KurboRect::new(0.0, 0.0, 640.0, 400.0);
            let drawn = DrawnFrame {
                transform: Affine::IDENTITY,
                window_rect: window,
                draw_us: 0,
                top_layer: None,
                tooltip: None,
                painted_over: false,
            };
            let mark = KurboRect::new(40.0, 40.0, 41.0, 56.0);
            let typed = KurboRect::new(40.0, 40.0, 160.0, 72.0);
            let mut damage = SurfaceDamage::default();
            let _ = damage.take();
            let presence = PresenceChange::Marks.with_paint(vec![mark]);
            damage.frame(None, &drawn, 1.0, None, presence, |_| {
                tilo_ui_base::scroll_copy::CopyPlan::default()
            });
            let _ = damage.take();
            // The presence is the same; a widget typed under the mark, which
            // moves it without presence changing.
            let same = PresenceChange::None.with_paint(vec![mark]);
            damage.frame(None, &drawn, 1.0, None, same, |regions| {
                regions.push(typed);
                tilo_ui_base::scroll_copy::CopyPlan::default()
            });
            let taken = damage.take().expect("the frame could be described");
            assert!(
                taken.iter().any(|rect| rect.contains(mark.center())),
                "the mark under the edit is not repainted: {taken:?}"
            );
            assert!(
                taken.iter().any(|rect| rect.contains(typed.center())),
                "the edit is not repainted: {taken:?}"
            );
        }
    }

    /// Records where it is clicked, in its own logical coordinates.
    struct ClickProbe {
        rect: KurboRect,
        pointer: Point,
        clicks: Rc<RefCell<Vec<Point>>>,
    }

    impl Widget for ClickProbe {
        fn observe_physical_input(&mut self, input: tilo_ui_base::widget::PhysicalInput) {
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
        tilo_ui_base::rect_geometry!();
        fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
            #[allow(
                unused_variables,
                unused_mut,
                reason = "a draw binds only the parts it uses"
            )]
            let (scene, transform, _text, theme, mut env) = ctx.parts();
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                theme.tokens.colors.surfaces.base,
                None,
                &self.rect,
            );
        }
        fn handle_event(
            &mut self,
            event: &WindowEvent,
            ctx: &mut tilo_ui_base::widget::EventCtx<'_>,
        ) {
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
        fn accessibility(&self) -> Option<tilo_ui_base::a11y::AccessNode> {
            let mut node = tilo_ui_base::a11y::AccessNode::new(tilo_ui_base::a11y::Role::Button);
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
        mut context: crate::application::ProcessContext,
        _: crate::application::LaunchTarget,
        _: crate::application::RestoreInput,
    ) -> Result<crate::application::AppComponent, crate::launch_error::LaunchError> {
        context
            .initialize_new_resource(|_| Ok(()))
            .map_err(crate::launch_error::LaunchError::Record)?;
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
        let host = crate::application_test_support::ProbeHarness::new();
        let definition = crate::application_test_support::definition(clicks_factory);
        let target = crate::application::LaunchTarget::New;
        let context = host
            .prepare_as(
                &definition,
                71,
                &target,
                tilo_ui_base::permissions::Role::Owner,
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
        let event =
            tilo_ui_base::synthetic_input::keyboard_input(winit::event::KeyEvent::synthetic(
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
            assert!(
                matches!(events[0], tilo_ui_base::widget::PhysicalInput::Key {
                key: observed, state: ElementState::Pressed
            } if observed == key)
            );
        });
        let mut synthetic = event;
        if let WindowEvent::KeyboardInput { is_synthetic, .. } = &mut synthetic {
            *is_synthetic = true;
        }
        assert!(tilo_ui_base::widget::PhysicalInput::from_window_event(&synthetic).is_none());
        app.surface_input(&region, synthetic, false);
        PHYSICAL.with_borrow(|events| assert_eq!(events.len(), 1));
        app.surface_input(&region, WindowEvent::Focused(false), false);
        PHYSICAL.with_borrow(|events| {
            assert!(matches!(
                events.last(),
                Some(tilo_ui_base::widget::PhysicalInput::Focused(false))
            ))
        });
    }

    /// A key press of `text` with the platform's command modifier held.
    fn command_key(app: &mut App, text: &str) -> WindowEvent {
        use winit::keyboard::{Key, KeyCode, KeyLocation, PhysicalKey};
        app.input.modifiers =
            winit::event::Modifiers::from(winit::keyboard::ModifiersState::CONTROL);
        app.input.platform = tilo_ui_base::platform::PlatformConventions::new(
            tilo_ui_base::platform::Platform::Linux,
        );
        tilo_ui_base::synthetic_input::keyboard_input(winit::event::KeyEvent::synthetic(
            winit::event::RawKeyEvent {
                physical_key: PhysicalKey::Code(KeyCode::Equal),
                state: ElementState::Pressed,
            },
            Key::Character(text.into()),
            None,
            KeyLocation::Standard,
            false,
            None,
            Key::Character(text.into()),
        ))
    }

    #[test]
    fn ctrl_plus_raises_the_content_scale_and_ctrl_0_resets_it() {
        let (mut app, _) = clicks_app();
        let region = Region {
            size: (320, 200),
            redraw: Cell::new(false),
        };
        app.surface_frame(&region, &mut Scene::new());
        let body = |app: &App| app.theme.text(tilo_ui_base::tokens::TextRole::Body).size;
        let (scale, text_scale, size) = (app.theme.ui_scale, app.theme.text_scale, body(&app));
        let gap = app.theme.tokens.spacing.s4;

        let plus = command_key(&mut app, "+");
        assert!(
            app.surface_input(&region, plus, false),
            "Ctrl+Plus is taken"
        );
        assert!(
            region.redraw.get(),
            "the window asks to be drawn at the new scale"
        );
        app.surface_frame(&region, &mut Scene::new());
        assert!(app.content_zoom() > 1.0, "Ctrl+Plus zoomed in");
        assert!(app.theme.ui_scale > scale, "the content scale rose");
        assert!(body(&app) > size, "text is set larger");
        assert!(
            app.theme.tokens.spacing.s4 > gap,
            "the layout reflows larger"
        );
        assert!(
            (app.theme.text_scale - text_scale).abs() < f32::EPSILON,
            "the system text size stays on top of the zoom"
        );
        assert!(
            app.accessibility.zoom.indicator_visible(crate::time::now()),
            "the level shows"
        );

        let reset = command_key(&mut app, "0");
        assert!(app.surface_input(&region, reset, false), "Ctrl+0 is taken");
        app.surface_frame(&region, &mut Scene::new());
        assert!(
            (app.content_zoom() - 1.0).abs() < f32::EPSILON,
            "Ctrl+0 reset the zoom"
        );
        assert!((app.theme.ui_scale - scale).abs() < 1e-6);
        assert!((body(&app) - size).abs() < 1e-3);

        let minus = command_key(&mut app, "-");
        assert!(app.surface_input(&region, minus, false));
        assert!(app.content_zoom() < 1.0, "Ctrl+Minus zoomed out");
    }

    /// Zoom is not a keyboard-only capability: the window offers Zoom in,
    /// Zoom out and Actual size as actions, to its palette and assistive
    /// technology and, published, to agents and scripts, and each runs the
    /// zoom its key does.
    #[test]
    fn zoom_is_an_action_of_every_window_not_only_its_keys() {
        use crate::host_chrome::zoom::ZOOM_RESOURCE;
        let (mut app, _) = clicks_app();
        let region = Region {
            size: (320, 200),
            redraw: Cell::new(false),
        };
        app.surface_frame(&region, &mut Scene::new());
        let scale = app.theme.ui_scale;
        let address = |action: &str| tilo_actions::ActionAddress::new(ZOOM_RESOURCE, action);
        let offered = |app: &mut App| {
            app.publication_resources()
                .iter()
                .any(|resource| resource.id.as_str() == ZOOM_RESOURCE)
        };
        assert!(
            !offered(&mut app),
            "an app with no window a person sees has nothing to zoom"
        );
        // The window opens, taking up the zoom its app had on this device.
        let dir = tempfile::tempdir().expect("a settings directory");
        app.accessibility.zoom.restore_from(
            crate::storage::Settings::for_app("so.tilo.zoom-actions").in_profile(dir.path()),
        );

        let local = app.history_local_resources();
        let zoom = local
            .iter()
            .find(|resource| resource.id.as_str() == ZOOM_RESOURCE)
            .expect("the window's own APIs offer zoom");
        let ids: Vec<_> = zoom
            .actions
            .iter()
            .map(|action| action.id.as_str())
            .collect();
        assert_eq!(ids, ["zoom-in", "zoom-out", "zoom-reset"]);
        assert_eq!(zoom.actions[0].label, "Zoom in");

        assert!(
            app.invoke_local_address(&address("zoom-in"), None),
            "the palette's and assistive technology's API runs Zoom in"
        );
        app.surface_frame(&region, &mut Scene::new());
        assert!(app.content_zoom() > 1.0, "Zoom in zoomed in");
        assert!(app.theme.ui_scale > scale, "the content scale rose");
        assert!(
            app.accessibility.zoom.indicator_visible(crate::time::now()),
            "the level shows, as it does for the keys"
        );

        assert!(app.invoke_local_address(&address("zoom-reset"), None));
        app.surface_frame(&region, &mut Scene::new());
        assert!(
            (app.content_zoom() - 1.0).abs() < f32::EPSILON,
            "Actual size reset the zoom"
        );
        assert!((app.theme.ui_scale - scale).abs() < 1e-6);

        assert!(
            offered(&mut app),
            "agents and scripts see the window's zoom"
        );
        let person = tilo_actions::Actor::Member(app.local_author.clone());
        app.invoke_resource_action(
            ResourceInvocation::new(&address("zoom-out"), &person).broker_admitted(true),
        )
        .expect("the person's own call through the broker zooms out");
        app.surface_frame(&region, &mut Scene::new());
        assert!(app.content_zoom() < 1.0, "Zoom out zoomed out");

        let before = app.content_zoom();
        assert!(
            app.invoke_resource_action(
                ResourceInvocation::new(
                    &address("zoom-in"),
                    &tilo_actions::Actor::Member(tilo_actions::MemberId::person("a-member"))
                )
                .broker_admitted(true),
            )
            .is_err(),
            "a member never zooms someone else's window"
        );
        assert!((app.content_zoom() - before).abs() < f32::EPSILON);
    }

    /// The close question on a phone's narrow window keeps its words and choices in its body.
    #[test]
    fn a_close_question_fits_a_phone() {
        let (mut app, _) = clicks_app();
        let region = Region {
            size: (360, 800),
            redraw: Cell::new(false),
        };
        app.surface_frame(&region, &mut Scene::new());
        use crate::close_admission::Choice;
        let choices = [Choice::Retry, Choice::Discard, Choice::KeepEditing];
        app.close_admission.show(
            "close-save-failed",
            "close-unsaved",
            &choices,
            app.input.platform,
        );
        app.layout.request_rebuild();
        app.surface_frame(&region, &mut Scene::new());
        let dialog = app.close_admission.dialog.as_ref().unwrap();
        let body = dialog.body_rect();
        let rows: Vec<_> = dialog
            .children()
            .iter()
            .flat_map(|c| c.children())
            .collect();
        assert!(!rows.is_empty());
        for row in rows {
            let rect = row.get_rect();
            assert!(
                rect.x0 >= body.x0 && rect.x1 <= body.x1,
                "{} at {rect:?} stands outside the sheet's body {body:?}",
                row.name()
            );
        }
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
            tilo_ui_base::synthetic_input::keyboard_input(winit::event::KeyEvent::synthetic(
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
                events.iter().all(|event| matches!(
                    event,
                    tilo_ui_base::widget::PhysicalInput::Focused(false)
                )),
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
                events.iter().all(|event| matches!(
                    event,
                    tilo_ui_base::widget::PhysicalInput::Focused(false)
                ))
            )
        });
        app.close_admission.dialog = None;
        app.surface_frame(&region, &mut Scene::new());
        PHYSICAL.with_borrow(|events| assert!(events.iter().any(|event|
            matches!(event, tilo_ui_base::widget::PhysicalInput::Layout(Some(layout)) if layout == &active))));
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
        let host = crate::application_test_support::ProbeHarness::new();
        let definition = crate::application_test_support::definition(clicks_factory);
        let target = crate::application::LaunchTarget::New;
        let context = host
            .prepare_as(
                &definition,
                72,
                &target,
                tilo_ui_base::permissions::Role::Owner,
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

    /// The palette fades in over the app outside its widget tree: the frame
    /// that starts it asks for the next, so a host that repaints the app
    /// only when asked (an embedded app) draws it arriving and at rest
    /// instead of leaving its first, nearly transparent pose on screen; once
    /// it rests the app asks for nothing.
    #[test]
    fn an_arriving_palette_asks_its_host_for_frames_until_it_rests() {
        let (mut app, _) = clicks_app();
        let region = Region {
            size: (480, 360),
            redraw: Cell::new(false),
        };
        let settle = |app: &mut App| {
            (0..8)
                .find(|_| !app.surface_frame(&region, &mut Scene::new()))
                .is_some()
        };
        assert!(settle(&mut app), "the app alone comes to rest");
        // A click opens it, so it moves (a key chord would land it at once).
        // Its frames run at fixed instants a frame apart, as a host presents
        // them: how long the test machine takes between them is not the
        // palette's time.
        let opened = crate::time::now();
        let next = opened + crate::time::Duration::from_millis(16);
        crate::time::testing::with_monotonic_time(opened, || {
            tilo_ui_base::motion::note_source(tilo_ui_base::tokens::MotionSource::Pointer);
            app.open_action_palette();
            let _ = app.surface_frame(&region, &mut Scene::new());
            assert!(app.palette_moving(), "a palette a click opened arrives");
        });
        crate::time::testing::with_monotonic_time(next, || {
            assert!(
                app.surface_frame(&region, &mut Scene::new()),
                "the arriving palette asks for the frame that draws it further in"
            );
        });
        let _settled = tilo_ui_base::motion::testing::settled();
        assert!(
            settle(&mut app),
            "a palette at rest asks for no more frames"
        );
    }

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
        embedded.draw(&mut tilo_ui_base::draw::DrawCtx::new(
            &mut scene,
            Affine::IDENTITY,
            &mut text,
            &Theme::default(),
            PresenceCtx::empty().draw_env(),
        ));
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
        let close = tilo_ui_base::synthetic_input::key_input(
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
        embedded.draw(&mut tilo_ui_base::draw::DrawCtx::new(
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            &Theme::default(),
            PresenceCtx::empty().draw_env(),
        ));

        fn find(
            node: &tilo_ui_base::a11y::AccessNode,
            name: &str,
        ) -> Option<tilo_ui_base::a11y::AccessNode> {
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
                &tilo_ui_base::synthetic_input::key_input(
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

    /// A palette a press opens (a control that needs confirmation, clicked
    /// in a window that does not hold the keyboard) stands over the window
    /// pressed in, not over the window holding focus. After a key, the
    /// window holding focus is the one that asked.
    #[test]
    fn a_palette_opened_by_a_press_stands_over_the_window_pressed_in() {
        let _settled = tilo_ui_base::motion::testing::settled();
        let mut app = App::new(
            crate::State::new(),
            Box::new(crate::testing::TwoWindows::new(["Chat", "Contacts"])),
        );
        let region = Region {
            size: (800, 300),
            redraw: Cell::new(false),
        };
        let _ = app.surface_frame(&region, &mut Scene::new());
        let device = winit::event::DeviceId::dummy();
        let contacts = KurboRect::new(400.0, 0.0, 800.0, 300.0);
        app.surface_input(
            &region,
            WindowEvent::CursorMoved {
                device_id: device,
                position: (600.0, 150.0).into(),
            },
            false,
        );
        app.surface_input(
            &region,
            WindowEvent::MouseInput {
                device_id: device,
                state: ElementState::Pressed,
                button: winit::event::MouseButton::Left,
            },
            false,
        );
        let _ = app.surface_frame(&region, &mut Scene::new());
        app.open_action_palette();
        assert_eq!(
            app.actions.palette_window,
            Some(contacts),
            "the palette stands over the window pressed in"
        );
        app.close_action_palette();

        app.input.pointer.pressed_at = None;
        let _ = app.surface_frame(&region, &mut Scene::new());
        app.open_action_palette();
        assert_ne!(
            app.actions.palette_window,
            Some(contacts),
            "after a key, the window pressed in no longer decides"
        );
    }
}
