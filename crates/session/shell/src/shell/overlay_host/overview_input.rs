#[allow(
    clippy::wildcard_imports,
    reason = "private implementation module shares its enclosing host types"
)]
use super::*;

impl ShellOverlayHost {
    pub(super) fn reconcile_overview_window_selection(&mut self) {
        if let Some(tab) = self.overview_window_selection
            && !self
                .overview_row_windows()
                .iter()
                .any(|window| window.address.tab == tab)
        {
            self.overview_window_selection = None;
            self.overview_window_interrupted = true;
            self.overview_selection = self.overview_here_index();
        }
    }

    /// Navigate the overview before input reaches content. Arrows keep the
    /// workspace ring; Tab includes creation, and Home/End reach its ends.
    pub(super) fn handle_overview_keyboard_nav(
        &mut self,
        logical_key: &Key,
        shift: bool,
        handled: &mut bool,
    ) -> bool {
        if !self.overlay_flags().overview || self.palette_visible {
            return false;
        }
        if !matches!(
            logical_key,
            Key::Named(
                NamedKey::ArrowDown
                    | NamedKey::ArrowUp
                    | NamedKey::ArrowLeft
                    | NamedKey::ArrowRight
                    | NamedKey::Tab
                    | NamedKey::Home
                    | NamedKey::End
                    | NamedKey::Enter
                    | NamedKey::Space
            )
        ) {
            return false;
        }
        self.reconcile_overview_window_selection();
        if std::mem::take(&mut self.overview_window_interrupted)
            && matches!(logical_key, Key::Named(NamedKey::Enter | NamedKey::Space))
        {
            *handled = true;
            return true;
        }
        if self.handle_overview_window_keys(logical_key) {
            *handled = true;
            return true;
        }
        if matches!(
            logical_key,
            Key::Named(NamedKey::ArrowLeft | NamedKey::ArrowRight)
        ) {
            return false;
        }
        self.overview_window_selection = None;
        self.overview_window_interrupted = false;
        let count = self.overview_cards().len();
        if count == 0 {
            return false;
        }
        match logical_key {
            Key::Named(NamedKey::Enter | NamedKey::Space) => {
                self.activate_overview_selection(count);
            }
            Key::Named(NamedKey::Home) => self.overview_selection = self.overview_here_index(),
            Key::Named(NamedKey::End) => self.overview_selection = Some(count.saturating_add(1)),
            _ => self.step_overview_selection(logical_key, shift, count),
        }
        self.scroll_overview_selection_into_view();
        self.bump_chrome();
        *handled = true;
        true
    }

    pub(super) fn step_overview_selection(&mut self, key: &Key, shift: bool, count: usize) {
        let tab = matches!(key, Key::Named(NamedKey::Tab));
        let back = matches!(key, Key::Named(NamedKey::ArrowUp)) || (tab && shift);
        let focus_count = if tab { count.saturating_add(2) } else { count };
        let mut current = self
            .overview_selection
            .or_else(|| self.overview_here_index())
            .unwrap_or(0);
        // Creation is outside the arrow-only workspace ring.
        if !tab && current >= count {
            current = if back { 0 } else { count.saturating_sub(1) };
        }
        let next = isize_of_usize(current)
            .wrapping_add(if back { -1 } else { 1 })
            .rem_euclid(isize_of_usize(focus_count));
        self.overview_selection = Some(usize_of_isize(next));
    }

    pub(super) fn activate_overview_selection(&mut self, count: usize) {
        let target = self
            .overview_selection
            .or_else(|| self.overview_here_index());
        if target == Some(count) {
            self.create_overview_workspace();
            self.latch_keyboard_overview();
            return;
        }
        if target == Some(count.saturating_add(1)) {
            self.open_join_sheet();
            self.latch_keyboard_overview();
            return;
        }
        if let (Some(index), Some(chrome)) = (target, self.chrome.clone()) {
            let mut state = chrome.borrow_mut();
            {
                let mut tree = state.tree_mut();
                if let Some(workspace) = tree.desktop.workspaces.get(index) {
                    let id = workspace.id;
                    tree.set_active(id);
                }
            }
            state.overlays.overview = false;
            state.bump();
            drop(state);
            self.latch_keyboard_overview();
        }
    }

    /// The pointer and keyboard release the overview in the same creation
    /// transaction, through the existing workspace model.
    pub(super) fn create_overview_workspace(&self) {
        if let Some(chrome) = self.chrome.as_ref() {
            let mut state = chrome.borrow_mut();
            state.overlays.overview = false;
            state.add_workspace("Untitled");
        }
    }

    /// Overview-only pointer and touch input: wheel pans the row list (the
    /// column header stays pinned); a click or tap on a window preview goes
    /// to that window, a middle click closes it and a drag carries it to
    /// another room; any other click or tap either creates a room (the "New
    /// room" slot) or activates the room under it and closes the overview.
    pub(super) fn handle_shell_overview_pointer_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        handled: &mut bool,
    ) -> bool {
        if !self.overlay_flags().overview {
            self.overview_press = None;
            return false;
        }
        if self.overview_naming_takes_press(event, scale_factor) {
            *handled = true;
            return true;
        }
        if self.handle_overview_press_event(event, scale_factor) {
            *handled = true;
            return true;
        }
        // Pans the overview's row list; the column header stays pinned.
        if let WindowEvent::MouseWheel { delta, .. } = event {
            let dy: f64 = match *delta {
                // Matches the strip's wheel factor (`GroupRow::handle_event`).
                winit::event::MouseScrollDelta::LineDelta(_, y) => -f64::from(y) * 30.0,
                winit::event::MouseScrollDelta::PixelDelta(p) => -p.y,
            };
            if dy.abs() > f64::EPSILON {
                let max = self.overview_max_scroll_y();
                self.overview_scroll_y = (self.overview_scroll_y + dy).clamp(0.0, max);
                *handled = true;
                return true;
            }
            return false;
        }
        // Click-on-overview-card: consume the click and — if it lands
        // on a workspace card — activate that workspace + close the
        // overview.
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: winit::event::MouseButton::Left,
            ..
        } = event
            && let Some(p) = self.last_cursor
        {
            self.overview_click_at(p);
            *handled = true;
            return true;
        }
        false
    }

    /// A click or tap on the overview away from the window previews:
    /// create a room on the "New room" slot, else go to the room under it;
    /// either way the overview closes.
    pub(super) fn overview_click_at(&mut self, p: Point) {
        // A strip's controls and name in the picture of this workspace.
        if self.overview_strip_click(p) {
            return;
        }
        // Wins the hit-test before the row-card scan so a
        // click on it doesn't fall through to "missed". Geometry, not the
        // painted cache: the open's first sample is still alpha 0 and
        // clears that cache.
        let add = self.add_workspace_hit_rect();
        if add.area() > 0.0 && add.contains(p) {
            self.create_overview_workspace();
            return;
        }
        let join = self.join_workspace_hit_rect();
        if join.area() > 0.0 && join.contains(p) {
            self.open_join_sheet();
            return;
        }
        let site_rooms = self.site_rooms();
        if !site_rooms.is_empty() {
            let leaving = (0..self.overview_cards().len()).find(|&idx| {
                let leave = self.overview_leave_rect(idx, &site_rooms);
                leave.area() > 0.0 && leave.contains(p)
            });
            if let (Some(idx), Some(chrome)) = (leaving, self.chrome.as_ref()) {
                let mut state = chrome.borrow_mut();
                let room = state
                    .tree()
                    .desktop
                    .workspaces
                    .get(idx)
                    .map(crate::shell::state::Workspace::room);
                if let Some(room) = room {
                    state.push_peer_sync_request(PeerSyncAction::LeaveSite { room });
                    state.bump();
                }
                return;
            }
        }
        let hit = self.hit_test_overview(p);
        if let Some(chrome) = self.chrome.clone() {
            let mut st = chrome.borrow_mut();
            if let Some(idx) = hit {
                let mut tree = st.tree_mut();
                if let Some(ws) = tree.desktop.workspaces.get(idx) {
                    let id = ws.id;
                    tree.set_active(id);
                }
            }
            // Dismisses even on a miss.
            st.overlays.overview = false;
            st.bump();
        }
    }

    /// The window whose preview tile is under `p`.
    pub(super) fn overview_window_at(&self, p: Point) -> Option<u64> {
        let index = self.hit_test_overview_window(p)?;
        self.overview_row_windows()
            .get(index)
            .map(|window| window.address.tab)
    }

    /// Presses that can turn into drags: the primary button on a window
    /// preview, and a finger anywhere on the overview. A middle click on a
    /// preview closes that window. Returns whether the event was taken.
    pub(super) fn handle_overview_press_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
    ) -> bool {
        use tilo_desktop::overview::{OverviewPress, PressSource};
        let taken = match event {
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: winit::event::MouseButton::Middle,
                ..
            } => match self.last_cursor.and_then(|p| self.overview_window_at(p)) {
                Some(tab) => {
                    self.close_overview_window(tab);
                    true
                }
                None => false,
            },
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: winit::event::MouseButton::Left,
                ..
            } => match self
                .last_cursor
                .and_then(|p| self.overview_window_at(p).map(|tab| (p, tab)))
            {
                Some((p, tab)) => {
                    self.overview_press =
                        Some(OverviewPress::new(PressSource::Mouse, Some(tab), p));
                    true
                }
                None => false,
            },
            WindowEvent::CursorMoved { .. } => {
                match (self.overview_press.as_mut(), self.last_cursor) {
                    (Some(press), Some(p)) if press.source == PressSource::Mouse => {
                        press.moved(p);
                        true
                    }
                    _ => false,
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: winit::event::MouseButton::Left,
                ..
            } => match self
                .overview_press
                .filter(|press| press.source == PressSource::Mouse)
            {
                Some(press) => {
                    self.overview_press = None;
                    let at = self.last_cursor.unwrap_or(press.at);
                    self.finish_overview_press(press.release(at));
                    true
                }
                None => false,
            },
            WindowEvent::Touch(touch) => {
                let p = Point::new(
                    touch.location.x / scale_factor,
                    touch.location.y / scale_factor,
                );
                let source = PressSource::Touch(touch.id);
                let mine = self
                    .overview_press
                    .is_some_and(|press| press.source == source);
                match touch.phase {
                    winit::event::TouchPhase::Started => {
                        // A second finger makes it a gesture, not a tap or
                        // a drag. A press the arena took over for a shell
                        // gesture never saw its end, so a new finger
                        // replaces it.
                        let mouse_held = self
                            .overview_press
                            .is_some_and(|press| press.source == PressSource::Mouse);
                        if !mouse_held {
                            self.overview_press = (self.touch_gestures.contact_count() <= 1)
                                .then(|| OverviewPress::new(source, self.overview_window_at(p), p));
                        }
                    }
                    winit::event::TouchPhase::Moved => {
                        if let Some(press) = self.overview_press.as_mut().filter(|_| mine) {
                            press.moved(p);
                        }
                    }
                    winit::event::TouchPhase::Ended => {
                        if let Some(press) = self.overview_press.filter(|_| mine) {
                            self.overview_press = None;
                            self.finish_overview_press(press.release(p));
                        }
                    }
                    winit::event::TouchPhase::Cancelled => {
                        if mine {
                            self.overview_press = None;
                        }
                    }
                }
                // The overview is modal: no finger reaches the desktop
                // beneath it.
                true
            }
            _ => false,
        };
        if taken && let Some(chrome) = self.chrome.as_ref() {
            chrome.borrow_mut().bump();
        }
        taken
    }

    /// Act on a press that was let go.
    pub(super) fn finish_overview_press(&mut self, end: tilo_desktop::overview::PressEnd) {
        use tilo_desktop::overview::PressEnd;
        match end {
            PressEnd::Tap { tab: Some(tab), at } => {
                if self.overview_window_at(at) == Some(tab) {
                    self.activate_overview_window(tab);
                }
            }
            PressEnd::Tap { tab: None, at } => self.overview_click_at(at),
            PressEnd::Drop { tab, at } => {
                if let Some(room) = self
                    .hit_test_overview(at)
                    .filter(|room| Some(*room) != self.overview_here_index())
                {
                    self.move_overview_window(tab, room);
                }
            }
            PressEnd::Nothing => {}
        }
    }
}
