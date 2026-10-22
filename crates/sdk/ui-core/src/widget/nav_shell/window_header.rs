//! The window's own controls as the shell hears of them: the bar's search
//! and History, the sync status and the window bar's geometry
//! ([`WindowHeader`]).

use super::NavShell;
use crate::widget::{HistoryControl, WindowHeader};

impl WindowHeader for NavShell {
    fn set_window_presents_search(&mut self, presents: bool) -> bool {
        if self.window_search != presents {
            self.window_search = presents;
            if presents {
                self.drop_app_search();
            }
            if self.inputs.borrow().slots_in_bar() {
                self.slots_changed();
                self.structure_dirty = true;
                self.invalidate();
            }
        }
        true
    }

    fn set_history_control(&mut self, control: Option<HistoryControl>) {
        if self.inputs.borrow().history == control {
            return;
        }
        self.inputs.borrow_mut().history = control;
        self.structure_dirty = true;
        self.invalidate();
    }

    fn set_sync_status(&mut self, status: Option<&crate::widget::WindowStatus>) -> bool {
        if self
            .sync_status
            .set(status.map(|status| status.text.as_str()))
        {
            self.status_dirty = true;
            self.invalidation.invalidate();
        }
        let action = status.and_then(|status| status.action.as_ref());
        if self.status_action_spec.as_ref() != action {
            self.status_action_spec = action.cloned();
            let next = action.map(crate::widget::StatusAction::button);
            // The host's tree may still point at the button it replaces
            // until it builds again.
            if let Some(old) = std::mem::replace(&mut self.status_action, next) {
                self.retired_status_actions.push(old);
            }
            self.inputs.borrow_mut().status_action = self.status_action.is_some();
            self.structure_dirty = true;
            self.invalidate();
        }
        true
    }

    fn set_window_bar(&mut self, bar: Option<crate::widget::WindowBar>) -> bool {
        let controls = bar.map(|bar| bar.controls_width);
        let revealed = bar.is_some_and(|bar| bar.revealed);
        let top = bar.map_or(0.0, |bar| bar.top_inset);
        let relayout = {
            let mut inputs = self.inputs.borrow_mut();
            let relayout = inputs.window_controls != controls
                || (inputs.header_in_bar
                    && (inputs.bar_revealed != revealed
                        || (inputs.bar_top - top).abs() > f64::EPSILON));
            inputs.window_controls = controls;
            inputs.bar_revealed = revealed;
            inputs.bar_top = top;
            relayout
        };
        if relayout {
            self.structure_dirty = true;
            self.invalidate();
        }
        true
    }

    fn window_bar_slot(&self) -> Option<crate::widget::WindowBarSlot> {
        let g = &self.geometry;
        (g.window_controls.width() > 0.0).then(|| crate::widget::WindowBarSlot {
            row: g.window_row,
            controls: g.window_controls,
            hides_with_bar: self.inputs.borrow().header_in_bar,
        })
    }
}
