//! Code's controls, as slots in the window's bar ([`ui::window_toolbar`]).
//!
//! The shell around the editor puts its sidebar toggle in the bar
//! ([`ui::widget::WindowBarLink`]). The editor adds the shown file as the
//! window's location and the bottom panel (Problems, Terminal, Search) as an
//! action. The window's search finds and replaces in the buffer
//! ([`super::find_workflow`]). The editor draws no header row of its own.

use ui::window_toolbar::{ToolbarAction, ToolbarEvent, ToolbarLocation, WindowToolbar};

use super::CodeApp;

/// The bar action that shows and hides the bottom panel.
pub(crate) const BOTTOM_PANEL: &str = "code.bottom-panel";

impl CodeApp {
    /// The window bar's slots for what the window shows now: the shell's,
    /// then the editor's.
    pub(crate) fn window_toolbar(&self) -> WindowToolbar {
        let mut toolbar = self
            .bar
            .as_ref()
            .map(ui::widget::WindowBarLink::slots)
            .unwrap_or_default();
        toolbar.location = self.session.active_buffer().map(|buffer| ToolbarLocation {
            label: buffer.name.clone(),
            edited: buffer.dirty,
            ..ToolbarLocation::default()
        });
        let panel = if self.panel_visible {
            "Hide panel"
        } else {
            "Show panel"
        };
        // A capture or a past version has no executor for the panel's
        // effect: the action shows, but cannot act.
        toolbar.actions.push(
            ToolbarAction::new(BOTTOM_PANEL, "panel-bottom", panel)
                .on(self.panel_visible)
                .enabled(self.live),
        );
        toolbar
    }

    /// Run what the person did in the window's bar, then declare the slots
    /// for what the window shows now.
    pub(super) fn run_window_toolbar(&mut self) {
        for event in ui::window_toolbar::take_events() {
            self.run_window_bar_event(&event);
        }
        ui::window_toolbar::declare(Some(self.window_toolbar()));
    }

    /// Run one thing the person did in the window's bar: the shell's own
    /// slots first, then the editor's.
    pub(super) fn run_window_bar_event(&mut self, event: &ToolbarEvent) {
        if self.bar.as_ref().is_some_and(|bar| bar.event(event)) {
            return;
        }
        if let ToolbarEvent::Action { id } = event
            && id == BOTTOM_PANEL
            && self.live
        {
            self.toggle_bottom_panel();
        }
    }
}

#[cfg(test)]
#[path = "window_bar_tests.rs"]
mod tests;
