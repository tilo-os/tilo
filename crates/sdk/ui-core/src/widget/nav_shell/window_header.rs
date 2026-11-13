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
            if self.inputs.borrow().toolbar_in_bar {
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
        let listed = bar.is_some_and(|bar| bar.lists_actions);
        let relayout = {
            let mut inputs = self.inputs.borrow_mut();
            let relayout = inputs.window_controls != controls || inputs.actions_listed != listed;
            inputs.window_controls = controls;
            inputs.actions_listed = listed;
            relayout
        };
        if relayout {
            self.structure_dirty = true;
            self.invalidate();
        }
        true
    }

    /// The row the shell draws at the window's top, while it draws one: the window's bar holds
    /// it while it can, so a row under the bar is a second one.
    fn header_row_height(&self) -> f64 {
        self.geometry.header_height
    }

    fn column_frames(&self) -> Vec<crate::widget::ColumnFrame> {
        let g = &self.geometry;
        super::Column::ALL
            .into_iter()
            .filter_map(|column| {
                let rect = g.column(column);
                (rect.width() > 0.0 && rect.height() > 0.0).then(|| crate::widget::ColumnFrame {
                    key: column.key(),
                    column: rect,
                    header: g.header(column),
                    rail: column == super::Column::Sidebar && g.sidebar_rail,
                })
            })
            .collect()
    }

    fn window_bar_slot(&self) -> Option<crate::widget::WindowBarSlot> {
        let g = &self.geometry;
        (g.window_controls.width() > 0.0).then_some(crate::widget::WindowBarSlot {
            row: g.window_row,
            controls: g.window_controls,
        })
    }
}
