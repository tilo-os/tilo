//! The presses of the bar's mouse buttons, each on what it lands on:
//! the left one presses and then lets go, the right one opens the menu of
//! what is under the pointer, and the middle one closes the tab it lands on.

use super::{ActionChip, Resist, TabBar};
use crate::widget::EventCtx;
use winit::event::{ElementState, MouseButton};

impl TabBar {
    /// A press or a release goes to the button it is: the right one opens the
    /// menu of what is under the pointer, the left one presses and then lets
    /// go, and the middle one closes the tab it lands on.
    pub(super) fn on_mouse_input(
        &mut self,
        state: ElementState,
        button: MouseButton,
        ctx: &mut EventCtx<'_>,
    ) {
        match (state, button) {
            (ElementState::Pressed, MouseButton::Right) => {
                // Works without first hovering to reveal the × — hosts actions
                // with no keyboard chord.
                if let Some(request) = self.context_menu_target(self.last_cursor) {
                    self.pending_context_menu = Some(request);
                    *ctx.handled = true;
                }
            }
            (ElementState::Pressed, MouseButton::Left) => {
                self.on_left_press(ctx.handled);
                // A press on one of the app's own slots is a press on the
                // app: the bar does not take the keyboard from it, so
                // Escape or what is typed next still reaches the app.
                if *ctx.handled && !self.toolbar_row.pressing_app_slot() {
                    *ctx.focus_target = Some(ctx.node_id);
                }
            }
            (ElementState::Pressed, MouseButton::Middle) => {
                // A middle press on a tab closes it: the × without aiming at
                // it, down to the same fade and the host's word on a tab
                // whose app has unsaved work.
                if let Some(id) = self.tab_under_middle_press() {
                    self.start_closing(id);
                    *ctx.handled = true;
                }
            }
            (ElementState::Released, MouseButton::Left) => self.finish_drag_release(ctx.handled),
            _ => {}
        }
    }

    /// Left-button press: fresh hit-tests against authoritative rects, not the
    /// `CursorMoved` hover cache, which can lag a click that arrives before a
    /// hover-refresh — otherwise the icon hit is swallowed and the tab just
    /// activates. Split out of `handle_event` to keep that dispatcher inside
    /// the `too_many_lines` budget.
    pub(super) fn on_left_press(&mut self, handled: &mut bool) {
        self.touch();
        let cursor_pos = self.last_cursor;
        if let Some(control) = self.controls.hit(cursor_pos) {
            self.toolbar_row.cancel_edit();
            self.pressed_control = Some(control);
            *handled = true;
            return;
        }
        if self.controls.on_pill(cursor_pos) {
            // The pill is the window's, padding included: a press on it is
            // the window's, never the app's underneath.
            self.toolbar_row.cancel_edit();
            *handled = true;
            return;
        }
        if self.history_action.area() > 0.0 && self.history_action.contains(cursor_pos) {
            self.toolbar_row.cancel_edit();
            self.history_action_pressed = true;
            *handled = true;
            return;
        }
        if self.press_opened_by(cursor_pos) {
            *handled = true;
            return;
        }
        if self.active_toolbar().is_some() && self.toolbar_row.press(cursor_pos) {
            *handled = true;
            return;
        }
        let icon_only: Vec<bool> = self.tabs.iter().map(|t| self.icon_only(t.id)).collect();
        let is_pinned_at = |i: usize| icon_only.get(i).copied().unwrap_or(false);
        let close_idx = (0..self.tabs.len())
            .find(|&i| {
                self.close_hit_rect(i)
                    .is_some_and(|r| r.contains(cursor_pos))
            })
            .filter(|&i| !is_pinned_at(i))
            .filter(|&i| self.tabs.get(i).is_none_or(|t| !self.is_held(t.id)));
        let action = if close_idx.is_some() {
            None
        } else {
            self.action_chip_at(cursor_pos)
                .filter(|&(i, _)| !is_pinned_at(i))
        };
        let max_idx = action
            .filter(|&(_, chip)| chip == ActionChip::Maximize)
            .map(|(i, _)| i);
        let search_idx = action
            .filter(|&(_, chip)| chip == ActionChip::Search)
            .map(|(i, _)| i);
        let history_idx = action
            .filter(|&(_, chip)| chip == ActionChip::History)
            .map(|(i, _)| i);

        // Sits in the bar's trailing space, also the whole-group drag-grab
        // zone, so it must win the press first.
        if self.new_tab_rect.area() > 0.0 && self.new_tab_rect.contains(cursor_pos) {
            self.pending_new_tab = Some(());
            *handled = true;
            return;
        }
        // A sound mark is a control of its own: it mutes, it does not select.
        let mark_hit = self.tabs.iter().zip(&self.mark_rects).find_map(|(t, r)| {
            let mark = self.marks.get(&t.id).copied()?;
            (mark.is_control() && r.area() > 0.0 && r.inflate(2.0, 2.0).contains(cursor_pos))
                .then_some((t.id, mark))
        });
        if let Some(hit) = mark_hit {
            self.pending_mark = Some(hit);
            *handled = true;
            return;
        }

        if let Some(ci) = close_idx
            && let Some(t) = self.tabs.get(ci)
        {
            let id = t.id;
            self.start_closing(id);
            *handled = true;
            return;
        }
        // Maximize icon — fires on first click regardless of whether
        // the tab is already active.
        if let Some(mi) = max_idx
            && let Some(t) = self.tabs.get(mi)
        {
            self.pending_max = Some(t.id);
            *handled = true;
            return;
        }
        // Search icon — same deal; shell drains `pending_search`.
        if let Some(si) = search_idx
            && let Some(t) = self.tabs.get(si)
        {
            // Deliberately does NOT activate the tab — chip
            // hits never bleed into selection, same as close
            // and maximize.
            self.pending_search = Some(t.id);
            *handled = true;
            return;
        }
        // History opens on the window it names: unlike the other chips it
        // selects its tab, so the History it opens is the one shown.
        if let Some(id) = history_idx.and_then(|hi| self.tabs.get(hi)).map(|t| t.id)
            && self.press_history(id)
        {
            *handled = true;
            return;
        }
        if let Some(idx) = self.hover_idx {
            // Can go stale if a neighbouring widget shrunk `tabs`/`tab_rects`
            // between move and press; clear so the next move repopulates it.
            let Some(pill) = self.tab_rects.get(idx).copied() else {
                self.hover_idx = None;
                return;
            };
            let origin = pill.center();
            self.press_origin = Some((idx, origin));
            if let Some(tab) = self.tabs.get(idx).filter(|t| self.is_held(t.id)) {
                self.resist = Some(Resist {
                    tab: tab.id,
                    origin: cursor_pos,
                    pull: vello::kurbo::Vec2::ZERO,
                    released_at: None,
                });
            }
            self.drag_offset = Some(vello::kurbo::Vec2::new(
                self.last_cursor.x - pill.x0,
                self.last_cursor.y - pill.y0,
            ));
            // Guard against `tabs`/`tab_rects` desyncing mid-frame.
            if let Some(tab) = self.tabs.get(idx) {
                self.active_id = tab.id;
                self.pending_activation = Some(tab.id);
            }
            *handled = true;
        }
    }
}
