//! The window's content zoom (`crate::content_zoom`), as an app window runs
//! it: a zoom gesture goes to a widget whose content zooms by itself
//! ([`crate::content_zoom::ContentZoom`]) first, then to the widgets as an
//! ordinary event, and what none of them took zooms the window's content.

use winit::event::WindowEvent;

use crate::content_zoom::{ZoomGesture, ZoomStep};

use super::App;

impl App {
    /// The window's content zoom factor (`1.0`: the default size).
    #[must_use]
    pub fn content_zoom(&self) -> f32 {
        self.accessibility.zoom.factor()
    }

    /// Take up the zoom this app's windows had on this device, and keep
    /// every later change there.
    pub fn restore_content_zoom(&mut self, app_id: &str) {
        if self.accessibility.zoom.restore_for_app(app_id) {
            self.content_zoom_changed();
        }
    }

    /// Zoom the window's content one `step`, as Ctrl+Plus, Ctrl+Minus and
    /// Ctrl+0 do where no widget zooms its own content. Returns whether
    /// the factor changed; the indicator shows either way.
    pub fn zoom_window_content(&mut self, step: ZoomStep) -> bool {
        let changed = self.accessibility.zoom.step(step, crate::time::now());
        if changed {
            self.content_zoom_changed();
        }
        changed
    }

    fn content_zoom_changed(&mut self) {
        self.rebuild_theme();
        self.layout.layout_dirty = true;
        self.layout.request_rebuild();
    }

    /// The zoom gesture `event` is, if it is one.
    fn zoom_gesture(&self, event: &WindowEvent) -> Option<ZoomGesture> {
        ZoomGesture::of(event, self.input.platform, self.input.modifiers.state())
    }

    /// Before the widgets see `event`: a widget whose content zooms by
    /// itself (a web page, per site) takes a zoom gesture, and the window
    /// shows the level it reports. Returns whether it took the event.
    pub(super) fn offer_content_zoom(&mut self, event: &WindowEvent) -> bool {
        let Some(gesture) = self.zoom_gesture(event) else {
            return false;
        };
        if !crate::content_zoom::provided(self.root_widget.as_mut()) {
            return false;
        }
        let Some(step) = self.accessibility.zoom.step_of(gesture) else {
            // A scroll or pinch still adding up to a step.
            return true;
        };
        match crate::content_zoom::offer(self.root_widget.as_mut(), step) {
            Some(factor) => self.accessibility.zoom.indicate(factor, crate::time::now()),
            // It has no zoom to offer now (a settings page over the web
            // page): the window zooms.
            None => {
                self.zoom_window_content(step);
            }
        }
        true
    }

    /// After the widgets left `event` alone: a zoom gesture zooms the
    /// window's content. Returns whether it was one.
    pub(super) fn zoom_unhandled(&mut self, event: &WindowEvent) -> bool {
        let Some(gesture) = self.zoom_gesture(event) else {
            return false;
        };
        if let Some(step) = self.accessibility.zoom.step_of(gesture) {
            self.zoom_window_content(step);
        }
        true
    }
}
