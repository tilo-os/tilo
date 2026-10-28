//! Driving an app that runs as a window of its own.
//!
//! The toolbar socket carries toolbar events, not pointer input, so a scene
//! that clicks inside an app window reaches the app's own widget tree
//! instead: the same cursor, press and release the compositor would deliver.

use super::ShellDriver;

impl ShellDriver {
    /// The app running as a window of its own on `surface_id`, for the
    /// steps that drive it as the person at its window would.
    pub fn own_process_app(
        &mut self,
        surface_id: u64,
    ) -> Option<&mut crate::own_process::OwnProcessApp> {
        self.own_process
            .iter_mut()
            .find(|app| app.surface_id == surface_id)
    }

    /// Where `target` sits inside the app window on `surface_id`, in that
    /// window's own pixels.
    pub fn own_process_target(
        &mut self,
        surface_id: u64,
        target: &str,
    ) -> Option<vello::kurbo::Point> {
        let app = self.own_process_app(surface_id)?;
        app.targets().get(target).map(vello::kurbo::Rect::center)
    }

    /// Click `target` inside the app window on `surface_id`: the pointer
    /// moves there, presses and releases, as at that point of the window.
    pub fn click_in_own_process_app(&mut self, surface_id: u64, target: &str) -> bool {
        let Some(point) = self.own_process_target(surface_id, target) else {
            return false;
        };
        let Some(app) = self.own_process_app(surface_id) else {
            return false;
        };
        for event in [
            ui::synthetic_input::cursor_moved(point.x, point.y),
            ui::synthetic_input::mouse_press(),
            ui::synthetic_input::mouse_release(),
        ] {
            app.send(&event);
        }
        true
    }

    /// Turn the wheel `dy` notches at the window's middle.
    pub fn scroll_in_own_process_app(&mut self, surface_id: u64, dy: f32) -> bool {
        let Some(app) = self.own_process_app(surface_id) else {
            return false;
        };
        let (w, h) = app.window_size();
        let middle = vello::kurbo::Point::new(f64::from(w) / 2.0, f64::from(h) / 2.0);
        for event in [
            ui::synthetic_input::cursor_moved(middle.x, middle.y),
            ui::synthetic_input::mouse_wheel(0.0, dy),
        ] {
            app.send(&event);
        }
        true
    }
}
