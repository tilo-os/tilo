//! Window focus, the strip pan, the window switcher and focused-group commands.

#[allow(
    clippy::wildcard_imports,
    reason = "private implementation module shares its enclosing host types"
)]
use super::*;

impl ShellOverlayHost {
    /// Selects `tab` inside `group` on the live widget tree (the
    /// authoritative side — `flush_active_tab_writeback` mirrors it into
    /// the persisted tree after input). Used by the compositor's Alt+Shift
    /// wheel jump.
    pub fn focus_window_widget(&mut self, strip_id: StripId, group: GroupId, tab: u64) -> bool {
        let Some(&ptr) = self.group_row_ptrs.get(&strip_id) else {
            return false;
        };
        // SAFETY: see field doc on `group_row_ptrs`.
        let row: &mut GroupRow = unsafe { &mut *ptr };
        row.set_group_active_tab(group, tab)
    }

    /// Directly pan the active strip from a three-finger gesture whose
    /// total `travel` was reported `at`. Focus is deliberately unchanged:
    /// moving the canvas and choosing an app are separate operations, just
    /// as they are during a pointer drag.
    pub fn update_active_strip_pan(&mut self, travel: f64, began: bool, at: Instant) -> bool {
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        let strip = {
            let st = chrome.borrow();
            st.tree()
                .desktop
                .active_strip
                .get(&st.tree().desktop.active)
                .copied()
        };
        let Some(strip) = strip else { return false };
        let Some(&ptr) = self.group_row_ptrs.get(&strip) else {
            return false;
        };
        // SAFETY: stable Box<GroupRow> contract documented on
        // `group_row_ptrs`; this method holds `&mut self` for the mutation.
        let row: &mut GroupRow = unsafe { &mut *ptr };
        // A strip showing a maximized window stays at home, as it does
        // under a wheel or a sideways pull: there is nothing beside the
        // window to pan to, and sliding it away is not restoring it.
        if row.groups.iter().any(|group| group.maximized) {
            return false;
        }
        if began {
            row.begin_strip_pan();
            self.strip_pan_active = true;
        }
        row.update_strip_pan(travel, at)
    }

    /// Finish direct strip manipulation at `now`: a strip the fingers threw
    /// coasts on, one released in elastic overpull settles to its end. A
    /// `cancelled` gesture throws nothing.
    pub fn end_active_strip_pan(&mut self, now: Instant, cancelled: bool) -> bool {
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        let strip = {
            let st = chrome.borrow();
            st.tree()
                .desktop
                .active_strip
                .get(&st.tree().desktop.active)
                .copied()
        };
        let Some(strip) = strip else { return false };
        let Some(&ptr) = self.group_row_ptrs.get(&strip) else {
            return false;
        };
        // SAFETY: same stable ownership contract as update above.
        let row: &mut GroupRow = unsafe { &mut *ptr };
        if cancelled {
            row.cancel_strip_pan(now)
        } else {
            row.end_strip_pan(now)
        }
    }

    /// Three fingers moving up or down: the strip deck follows them.
    /// `travel` is their total travel since the gesture began (positive
    /// toward the strips below), reported `at`; `began` on its first
    /// report, which catches the deck where it is, mid-slide included.
    pub fn update_strip_deck_drag(&mut self, travel: f64, began: bool, at: Instant) -> bool {
        let Some(deck) = self
            .desktop_surface_mut()
            .and_then(tilo_desktop::composition::DesktopSurface::strip_deck_mut)
        else {
            return false;
        };
        if began {
            deck.begin_drag(at);
        }
        deck.drag_to(travel, at)
    }

    /// The fingers on the strip deck lifted at `now` (or the gesture was
    /// `cancelled`): the deck settles with their momentum on the strip the
    /// swipe carried it to, and that strip takes focus, as Ctrl+Super+↑ / ↓
    /// would have given it.
    pub fn end_strip_deck_drag(&mut self, now: Instant, cancelled: bool) -> bool {
        let Some(release) = self
            .desktop_surface_mut()
            .and_then(tilo_desktop::composition::DesktopSurface::strip_deck_mut)
            .and_then(|deck| deck.end_drag(now, cancelled))
        else {
            return false;
        };
        if release.to != release.from
            && let Some(chrome) = self.chrome.clone()
        {
            ui::motion::note_source(ui::tokens::MotionSource::Gesture);
            let _ = chrome.borrow_mut().focus_strip_index(release.to);
        }
        true
    }

    /// Moves focus one or more windows along the active strip — the single
    /// implementation behind keyboard, scroll, touchpad and modifier-wheel
    /// navigation, so they can't drift into models that disagree at group
    /// boundaries. A "window" is a tab in one flat sequence across groups
    /// (stepping past a group's last tab continues into the next rather
    /// than wrapping), clamped at the strip's ends. Widget written before
    /// tree, per [`Self::focus_window_widget`].
    pub fn step_focus_along_strip(&mut self, steps: i32) -> bool {
        if steps == 0 {
            return false;
        }
        if self
            .chrome
            .as_ref()
            .is_some_and(|c| c.borrow().axis_navigation_enabled())
        {
            return self.step_axis_navigation(steps);
        }
        if let Some(moved) = self.step_focus_across_displays(steps) {
            return moved;
        }
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        let target = {
            let st = chrome.borrow();
            let tree = st.tree();
            crate::shell::input_policy::step_active_window(&tree, steps)
        };
        let Some((strip, group, tab)) = target else {
            return false;
        };
        self.focus_window_on_strip(strip, group, tab)
    }

    /// The Alt+Tab switcher, for input routing, state readers and tests.
    pub fn window_switcher(&self) -> &tilo_desktop::window_switcher::WindowSwitcher {
        &self.layers.window_switcher
    }

    pub fn window_switcher_mut(&mut self) -> &mut tilo_desktop::window_switcher::WindowSwitcher {
        &mut self.layers.window_switcher
    }

    /// Alt is held after Alt+Tab: the shell owns the keyboard and pointer
    /// until the switcher commits or cancels.
    pub fn window_switcher_active(&self) -> bool {
        self.layers.window_switcher.is_active()
    }

    /// The windows whose offscreen previews chrome is showing or about to
    /// show: the open switcher's (from the first Alt+Tab, so they are ready
    /// when it appears). The overview asks for none: it draws every window
    /// live in the frame that shows it. Nothing while the session is
    /// locked.
    pub(super) fn window_preview_targets(
        &self,
    ) -> Vec<tilo_desktop::window_switcher::PreviewRequest> {
        let Some(chrome) = self.chrome.as_ref() else {
            return Vec::new();
        };
        let Ok(state) = chrome.try_borrow() else {
            return Vec::new();
        };
        if state.session_locked() || self.onboarding.is_some() {
            return Vec::new();
        }
        if self.layers.window_switcher.is_active() {
            return self.layers.window_switcher.preview_requests();
        }
        Vec::new()
    }

    /// Start a preview refresh round at `now`: what the compositor should
    /// render this round, or nothing when the round is not due. Drops
    /// every preview as soon as no chrome shows them.
    pub fn window_preview_requests(
        &mut self,
        now: Instant,
    ) -> Vec<tilo_desktop::window_switcher::PreviewRequest> {
        let wanted = self.window_preview_targets();
        if wanted.is_empty() {
            self.window_previews.clear();
            return Vec::new();
        }
        self.window_previews.begin_refresh(now, &wanted)
    }

    /// Whether any chrome shows window previews now.
    pub fn wants_window_previews(&self) -> bool {
        !self.window_preview_targets().is_empty()
    }

    /// The compositor went through `visited` requests of this round.
    pub fn advance_window_previews(&mut self, visited: usize) {
        self.window_previews.advance(visited);
    }

    /// The client frame the preview of `tab` was rendered from.
    pub fn window_preview_stamp(&self, tab: u64) -> Option<u64> {
        self.window_previews.stamp(tab)
    }

    /// Keep a freshly rendered preview of `tab`.
    pub fn store_window_preview(
        &mut self,
        tab: u64,
        rgba: std::sync::Arc<[u8]>,
        width: u32,
        height: u32,
        stamp: Option<u64>,
    ) {
        self.window_previews.store(tab, rgba, width, height, stamp);
    }

    /// Each window the overview's row last drew and what its well showed,
    /// for readers and tests.
    pub fn overview_row_drawn(&self) -> &[(u64, tilo_desktop::window_switcher::PreviewState)] {
        &self.overview_row_drawn
    }

    /// The previews on hand, for readers and tests.
    pub fn window_previews(&self) -> &crate::shell::window_previews::WindowPreviews {
        &self.window_previews
    }

    /// When the next preview round is due, while any chrome wants one.
    pub(super) fn window_previews_wake_deadline(&self, now: Instant) -> Option<Instant> {
        if self.window_preview_targets().is_empty() {
            return None;
        }
        Some(self.window_previews.next_refresh_at().unwrap_or(now))
    }

    /// The switcher as `state-dump` reports it: its windows, which is
    /// selected, and whether each card shows a live preview.
    /// Whether the power menu is open, for `state_dump`.
    pub fn power_menu_open(&self) -> bool {
        self.layers.power_menu.is_open()
    }

    pub fn window_switcher_state_json(&self) -> serde_json::Value {
        self.layers
            .window_switcher
            .state_json(&self.window_previews, Instant::now())
    }

    /// Record the focused window as the most recently used one.
    pub(super) fn observe_window_focus(&mut self) {
        let Some(chrome) = self.chrome.as_ref() else {
            return;
        };
        let recent = match chrome.try_borrow() {
            Ok(state) => {
                let tree = state.tree();
                self.layers.window_switcher.observe(&tree.desktop);
                self.layers.window_switcher.recent_tabs(&tree.desktop)
            }
            Err(_) => return,
        };
        let dictating = self.layers.dictation.session().is_some();
        if let Ok(mut state) = chrome.try_borrow_mut()
            && (state.search_context.recent_windows != recent
                || state.search_context.dictating != dictating)
        {
            state.search_context.recent_windows = recent;
            state.search_context.dictating = dictating;
        }
    }

    /// Alt+Tab or Alt+Grave: open the switcher on its first step.
    pub(super) fn begin_window_switcher(
        &mut self,
        scope: tilo_desktop::window_switcher::SwitcherScope,
        backwards: bool,
    ) -> bool {
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        self.layers
            .window_switcher
            .set_host_rect(self.overlay_area());
        let began = {
            let state = chrome.borrow();
            let tree = state.tree();
            self.layers.window_switcher.begin(
                &tree.desktop,
                &crate::shell::window_previews::ShellWindows,
                scope,
                backwards,
                Instant::now(),
            )
        };
        if began {
            chrome.borrow_mut().bump();
        }
        began
    }

    /// Select the physical keyboard whose sources the switch shortcut cycles.
    pub(crate) fn set_keyboard_scope(&self, scope: String) {
        if let Some(chrome) = self.chrome.as_ref() {
            chrome.borrow_mut().set_keyboard_scope(scope);
        }
    }

    /// The modifiers held changed: the palette's selected row previews
    /// what Enter would do with them. Whether that needs a new frame.
    pub fn note_held_modifiers(&mut self, modifiers: winit::keyboard::ModifiersState) -> bool {
        self.palette.set_held_modifiers(modifiers) && self.palette_visible
    }

    /// The modifier state changed with no key event to carry it (the
    /// control socket's modifier command, a backend's `ModifiersChanged`).
    /// A switcher held open by Alt commits once Alt is up.
    pub fn note_modifiers(&mut self, modifiers: winit::keyboard::ModifiersState) -> bool {
        let outcome = self.layers.window_switcher.modifiers_changed(modifiers);
        switcher_layer::outcome_intent(outcome).is_some_and(|intent| {
            self.apply_layer_intent(intent);
            true
        })
    }

    /// Focus a window the switcher chose, on whichever strip of the active
    /// workspace it lives. A window closed while Alt was held is skipped.
    pub(super) fn focus_switcher_window(
        &mut self,
        address: tilo_desktop::window_switcher::WindowAddress,
    ) -> bool {
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        let workspace = {
            let state = chrome.borrow();
            let tree = state.tree();
            let live = tree
                .desktop
                .active_workspace()
                .and_then(|ws| ws.strips.iter().find(|strip| strip.id == address.strip))
                .and_then(|strip| {
                    strip
                        .window_groups
                        .iter()
                        .find(|group| group.id == address.group)
                })
                .is_some_and(|group| group.tabs.iter().any(|tab| tab.id == address.tab));
            live.then_some(tree.desktop.active)
        };
        let Some(workspace) = workspace else {
            chrome.borrow_mut().bump();
            return false;
        };
        let _ = self.focus_window_widget(address.strip, address.group, address.tab);
        chrome
            .borrow_mut()
            .select_tab(workspace, address.strip, address.group, address.tab);
        self.observe_window_focus();
        true
    }

    /// Cycle tabs inside the focused window group. Unlike strip focus, this
    /// wraps because the tab bar is a small local ring.
    pub fn cycle_focused_tab(&mut self, steps: i32) -> bool {
        if steps == 0 {
            return false;
        }
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        let target = (|| -> Option<(StripId, GroupId, u64)> {
            let st = chrome.borrow();
            let tree = st.tree();
            let strip = tree
                .desktop
                .active_strip
                .get(&tree.desktop.active)
                .copied()?;
            let group_id = tree.desktop.active_group_for(strip)?;
            let group = tree
                .desktop
                .active_workspace()?
                .strips
                .iter()
                .find(|candidate| candidate.id == strip)?
                .window_groups
                .iter()
                .find(|candidate| candidate.id == group_id)?;
            let current = group
                .tabs
                .iter()
                .position(|tab| tab.id == group.active_tab)
                .unwrap_or(0);
            let len = isize::try_from(group.tabs.len()).ok()?;
            if len == 0 {
                return None;
            }
            let next = isize::try_from(current)
                .ok()?
                .checked_add(isize::try_from(steps).ok()?)?
                .rem_euclid(len);
            let target = (
                strip,
                group_id,
                group.tabs.get(usize::try_from(next).ok()?)?.id,
            );
            drop(tree);
            drop(st);
            Some(target)
        })();
        let Some(target) = target else {
            return false;
        };
        self.focus_window_on_strip(target.0, target.1, target.2)
    }

    /// Resize the focused group in stable five-percent increments.
    pub fn resize_focused_group(&mut self, delta_permille: i16) -> bool {
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        let (strip_id, group_id, width) = {
            let st = chrome.borrow_mut();
            let mut tree = st.tree_mut();
            let Some(strip_id) = tree.desktop.active_strip.get(&tree.desktop.active).copied()
            else {
                return false;
            };
            let Some(group_id) = tree.desktop.active_group_for(strip_id) else {
                return false;
            };
            let Some(group) = tree
                .desktop
                .active_workspace_mut()
                .and_then(|workspace| {
                    workspace
                        .strips
                        .iter_mut()
                        .find(|strip| strip.id == strip_id)
                })
                .and_then(|strip| {
                    strip
                        .window_groups
                        .iter_mut()
                        .find(|group| group.id == group_id)
                })
            else {
                return false;
            };
            let next = u32::try_from(
                i64::from(group.width.permille())
                    .saturating_add(i64::from(delta_permille))
                    .clamp(
                        i64::from(ui::widget::WIDTH_MIN_PERMILLE),
                        i64::from(ui::widget::WIDTH_MAX_PERMILLE),
                    ),
            )
            .unwrap_or(ui::widget::WIDTH_MAX_PERMILLE);
            if next == group.width.permille() {
                return false;
            }
            group.width = crate::shell::state::WindowWidth::Fraction(next);
            tree.queue_save();
            let result = (strip_id, group_id, next);
            drop(tree);
            drop(st);
            result
        };
        for &ptr in self.group_row_ptrs.values() {
            // SAFETY: see field doc on `group_row_ptrs`.
            let row: &mut GroupRow = unsafe { &mut *ptr };
            if let Some(group) = row.groups.iter_mut().find(|group| group.id == group_id) {
                group.set_width(ui::widget::WindowWidth::Fraction(width));
                break;
            }
        }
        if let Some(surface) = self.desktop_surface_mut() {
            surface.reveal_on_next_layout(strip_id);
        }
        chrome.borrow_mut().bump();
        true
    }

    /// `group` was just focused by a click or touch on what it shows: keep
    /// the strip where it is, as a click on its chrome does. The person
    /// aimed at something on screen; panning to frame the whole window would
    /// slide it out from under the pointer, and the strip would seem to move
    /// on its own.
    pub fn keep_clicked_group_in_place(&mut self, strip: StripId, group: u64) {
        if let Some(surface) = self.desktop_surface_mut() {
            surface.keep_clicked_group_in_place(strip, group);
        }
    }

    /// Pan `strip` at once, without easing, so `group` is wholly on screen,
    /// and keep it framed there. For an agent acting in one window: it sees
    /// and aims at the whole window on this command, not after an animation.
    /// `false` when the strip has no row or the group was already in view.
    pub fn reveal_group_now(&mut self, strip: StripId, group: u64) -> bool {
        let Some(&ptr) = self.group_row_ptrs.get(&strip) else {
            return false;
        };
        // SAFETY: see field doc on `group_row_ptrs`.
        let row: &mut GroupRow = unsafe { &mut *ptr };
        let moved = row.scroll_group_into_view(group);
        if let Some(surface) = self.desktop_surface_mut() {
            surface.keep_clicked_group_in_place(strip, group);
        }
        moved
    }

    /// Move the focused group one slot while preserving its contents and
    /// active tab. This is the keyboard twin of dragging its tab-bar ground.
    pub fn reorder_focused_group(&mut self, direction: i32) -> bool {
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        let (strip_id, group_id, current, destination) =
            {
                let st = chrome.borrow();
                let tree = st.tree();
                let Some(strip_id) = tree.desktop.active_strip.get(&tree.desktop.active).copied()
                else {
                    return false;
                };
                let Some(group_id) = tree.desktop.active_group_for(strip_id) else {
                    return false;
                };
                let Some(strip) = tree.desktop.active_workspace().and_then(|workspace| {
                    workspace.strips.iter().find(|strip| strip.id == strip_id)
                }) else {
                    return false;
                };
                let Some(current) = strip
                    .window_groups
                    .iter()
                    .position(|group| group.id == group_id)
                else {
                    return false;
                };
                let last = strip.window_groups.len().saturating_sub(1);
                // On a partitioned strip the neighbour is the next group
                // on the same display; groups in between belong elsewhere.
                let partition = &tree.desktop.outputs;
                let display = partition.display_of(group_id);
                let same_display = |g: &crate::shell::state::WindowGroup| {
                    partition.displays.len() < 2 || partition.display_of(g.id) == display
                };
                let destination = if direction < 0 {
                    strip
                        .window_groups
                        .get(..current)
                        .and_then(|before| before.iter().rposition(same_display))
                        .unwrap_or(current)
                } else {
                    strip
                        .window_groups
                        .get(current.saturating_add(1)..)
                        .and_then(|rest| rest.iter().position(same_display))
                        .map_or(current, |i| current.saturating_add(1).saturating_add(i))
                        .min(last)
                };
                drop(tree);
                (strip_id, group_id, current, destination)
            };
        if destination == current && self.is_partitioned() {
            return self.carry_focused_group_across_seam(direction);
        }
        let reorder = tilo_desktop::intents::SessionIntent::ReorderGroup {
            strip: strip_id,
            group: group_id,
            to: destination,
        };
        let changed = destination != current
            && Self::dispatch_desktop_intents(
                &chrome,
                [tilo_desktop::intents::DesktopIntent::Session(reorder)],
            ) > 0;
        if changed {
            if let Some(surface) = self.desktop_surface_mut() {
                surface.reveal_on_next_layout(strip_id);
            }
            chrome.borrow_mut().bump();
        }
        changed
    }

    /// Carry the focused group, whole, to the strip above (`direction < 0`)
    /// or below, landing after that strip's focused group; focus follows
    /// it. Clamps at the first and last strip rather than creating one.
    pub fn move_focused_group_to_strip(&mut self, direction: i32) -> bool {
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        let planned = (|| {
            let st = chrome.borrow();
            let tree = st.tree();
            let source = tree
                .desktop
                .active_strip
                .get(&tree.desktop.active)
                .copied()?;
            let group = tree.desktop.active_group_for(source)?;
            let strips = &tree.desktop.active_workspace()?.strips;
            let from = strips.iter().position(|strip| strip.id == source)?;
            let to = match direction.signum() {
                -1 => from.checked_sub(1)?,
                1 => from.checked_add(1).filter(|to| *to < strips.len())?,
                _ => return None,
            };
            let destination = strips.get(to)?.id;
            drop(tree);
            drop(st);
            Some((group, destination))
        })();
        let Some((group, destination)) = planned else {
            return false;
        };
        self.move_group_to_strip(group, destination)
    }

    /// Carry `group`, whole, into `destination`, another strip of its
    /// workspace, landing after that strip's focused group; focus follows
    /// it, and the strip it left focuses its neighbour. What the
    /// move-to-strip shortcuts and the window menu's Move to strip do.
    pub(crate) fn move_group_to_strip(&mut self, group: GroupId, destination: StripId) -> bool {
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        let planned = (|| {
            let st = chrome.borrow();
            let tree = st.tree();
            let workspace = tree.desktop.workspaces.iter().find(|workspace| {
                workspace
                    .strips
                    .iter()
                    .any(|strip| strip.window_groups.iter().any(|g| g.id == group))
            })?;
            let source_strip = workspace
                .strips
                .iter()
                .find(|strip| strip.window_groups.iter().any(|g| g.id == group))?;
            if source_strip.id == destination {
                return None;
            }
            let destination = workspace
                .strips
                .iter()
                .find(|strip| strip.id == destination)?;
            let source_groups = &source_strip.window_groups;
            let at = source_groups.iter().position(|g| g.id == group)?;
            let heir = source_groups
                .get(at.saturating_add(1))
                .or_else(|| at.checked_sub(1).and_then(|left| source_groups.get(left)))
                .map(|g| g.id);
            let anchor = tree
                .desktop
                .active_group_for(destination.id)
                .and_then(|id| destination.window_groups.iter().position(|g| g.id == id));
            let before = anchor
                .and_then(|i| destination.window_groups.get(i.saturating_add(1)))
                .map(|g| g.id);
            let plan = (source_strip.id, heir, destination.id, before);
            drop(tree);
            drop(st);
            Some(plan)
        })();
        let Some((source, heir, destination, before)) = planned else {
            return false;
        };
        let carry = tilo_desktop::intents::SessionIntent::MoveGroup {
            group,
            to: destination,
            before,
        };
        if Self::dispatch_desktop_intents(
            &chrome,
            [tilo_desktop::intents::DesktopIntent::Session(carry)],
        ) == 0
        {
            return false;
        }
        {
            let mut st = chrome.borrow_mut();
            {
                let mut tree = st.tree_mut();
                tree.desktop.set_active_group(destination, group);
                if let Some(heir) = heir {
                    tree.desktop.set_active_group(source, heir);
                }
                tree.queue_save();
            }
            st.focus_strip(destination);
            st.bump();
        }
        if let Some(surface) = self.desktop_surface_mut() {
            surface.reveal_on_next_layout(destination);
        }
        true
    }

    #[allow(
        clippy::needless_pass_by_ref_mut,
        reason = "the row is mutated through this host's owned GroupRow pointer; the exclusive borrow keeps that mutation unaliased"
    )]
    pub(super) fn mutate_focused_group_widget(
        &mut self,
        action: impl FnOnce(&mut GroupRow, GroupId) -> bool,
    ) -> bool {
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        let focused = {
            let state = chrome.borrow();
            let tree = state.tree();
            let strip = tree.desktop.active_strip.get(&tree.desktop.active).copied();
            let group = strip.and_then(|strip| tree.desktop.active_group_for(strip));
            drop(tree);
            drop(state);
            strip.zip(group)
        };
        let Some((strip, group)) = focused else {
            return false;
        };
        let Some(&ptr) = self.group_row_ptrs.get(&strip) else {
            return false;
        };
        // SAFETY: stable GroupRow pointer owned by this host's content tree.
        let row: &mut GroupRow = unsafe { &mut *ptr };
        action(row, group)
    }

    pub(crate) fn toggle_focused_group_maximized(&mut self) -> bool {
        self.mutate_focused_group_widget(|row, group| {
            let Some(group) = row
                .groups
                .iter_mut()
                .find(|candidate| candidate.id == group)
            else {
                return false;
            };
            group.toggle_maximized();
            true
        })
    }

    /// Maximize or restore the focused group; `false` when it already is in
    /// that state or nothing is focused.
    pub(crate) fn set_focused_group_maximized(&mut self, maximized: bool) -> bool {
        self.mutate_focused_group_widget(|row, group| {
            let Some(group) = row
                .groups
                .iter_mut()
                .find(|candidate| candidate.id == group)
            else {
                return false;
            };
            if group.maximized == maximized {
                return false;
            }
            group.toggle_maximized();
            true
        })
    }

    /// Super+F on a Tilo window: cover the output, including the wheel.
    /// A client `xdg_toplevel` fullscreen still goes through the compositor.
    pub(crate) fn toggle_focused_group_fullscreen(&self) -> bool {
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        let focused = {
            let state = chrome.borrow();
            let tree = state.tree();
            let strip = tree.desktop.active_strip.get(&tree.desktop.active).copied();
            let group = strip.and_then(|strip| tree.desktop.active_group_for(strip));
            drop(tree);
            drop(state);
            group
        };
        let Some(group) = focused else {
            return false;
        };
        let current = chrome.borrow().fullscreen_group();
        chrome
            .borrow_mut()
            .set_user_fullscreen_group(if current == Some(group) {
                None
            } else {
                Some(group)
            });
        true
    }

    pub(super) fn detach_focused_tab(&mut self) -> bool {
        self.mutate_focused_group_widget(GroupRow::detach_active_tab)
    }

    pub(super) fn attach_focused_tab(&mut self, direction: i32) -> bool {
        self.mutate_focused_group_widget(|row, group| row.attach_active_tab(group, direction))
    }

    /// Focuses one window by identity, then pans the strip to it (unlike a
    /// click on the window itself) since the rail addresses windows that may
    /// be off screen.
    pub fn focus_window_on_strip(&mut self, strip: StripId, group: GroupId, tab: u64) -> bool {
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        // Both the durable tree and the live row have to still address the
        // target. A rail/keyboard event can race a close in either
        // direction: before resync the widget still has the old tab, and
        // after resync an event can still carry its old id. Do not stamp an
        // absent address into the persistent focus pointers in either case.
        let tree_has_target = {
            let state = chrome.borrow();
            state
                .tree()
                .desktop
                .active_workspace()
                .and_then(|workspace| {
                    workspace
                        .strips
                        .iter()
                        .find(|candidate| candidate.id == strip)
                })
                .and_then(|strip| {
                    strip
                        .window_groups
                        .iter()
                        .find(|candidate| candidate.id == group)
                })
                .is_some_and(|group| group.tabs.iter().any(|candidate| candidate.id == tab))
        };
        if !tree_has_target || !self.focus_window_widget(strip, group, tab) {
            return false;
        }
        let mut changed = false;
        {
            let st = chrome.borrow_mut();
            let mut tree = st.tree_mut();
            if let Some(g) = tree
                .desktop
                .active_workspace_mut()
                .and_then(|ws| ws.strips.iter_mut().find(|s| s.id == strip))
                .and_then(|s| s.window_groups.iter_mut().find(|g| g.id == group))
                && g.active_tab != tab
            {
                g.activate_tab(tab);
                changed = true;
            }
            if tree.desktop.active_group_for(strip) != Some(group) {
                tree.desktop.set_active_group(strip, group);
                changed = true;
            }
            let workspace = tree.desktop.active;
            if tree.desktop.active_strip.get(&workspace).copied() != Some(strip) {
                tree.desktop.active_strip.insert(workspace, strip);
                changed = true;
            }
            if changed {
                tree.queue_save();
            }
        }
        if changed {
            chrome.borrow_mut().bump();
        }
        self.observe_window_focus();
        changed
    }

    /// Focus the first window of `group` — what clicking a rail segment
    /// means, since a segment stands for a whole group.
    pub fn focus_group_on_strip(&mut self, strip: StripId, group: GroupId) -> bool {
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        let tab = {
            let st = chrome.borrow();
            let tree = st.tree();
            tree.desktop
                .active_workspace()
                .and_then(|ws| ws.strips.iter().find(|s| s.id == strip))
                .and_then(|s| s.window_groups.iter().find(|g| g.id == group))
                .map(|g| g.active_tab)
        };
        let Some(tab) = tab else { return false };
        self.focus_window_on_strip(strip, group, tab)
    }
}
