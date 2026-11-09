//! The session puts up what draws the shell's window menu slot: the view the
//! person bound to it, a tilolang package run here or a program of its own,
//! and serves what that view asks through the session's door.

use tilo_actions::ActionAddress;

use crate::{Session, mount, surfaces};
use tilo_compositor::Error;

impl Session {
    /// Put up the view that draws the shell's window menu, and show it while
    /// the reading says a menu is up: the view the person bound to the slot,
    /// or the one the session ships with. Many a machine has no such view
    /// installed, or another one bound in its place: the menu is a reading
    /// either way, and this only puts it on the screen.
    ///
    /// The view runs for the session's life, standing hidden over the desk
    /// between openings: an app takes a moment to start and connect, and a
    /// menu that waits for that is a menu the person pressed for and did not
    /// see. Showing the surface it already stands on costs no start.
    ///
    /// A binding whose app is a tilolang package runs here, over the reading
    /// the session publishes for the slot; any other is a program of its own.
    ///
    /// Rebinding the slot starts the view bound to it now, on the next pass,
    /// hidden or over the reading it already drew.
    pub(crate) fn follow_window_menu(&mut self) {
        use mount::slots::{Mount, View};

        /// The view's surface name, whatever view draws it.
        const VIEW: &str = mount::shell::WINDOW_MENU_SLOT;
        /// The app whose view the session shows the menu through until the person binds another.
        const SHIPPED: (&str, &str) = ("so.tilo.window-menu", "window-menu");
        // Nothing to draw or to watch until the session publishes the menu.
        let Some(reading) = self.read(mount::shell::WINDOW_MENU) else {
            return;
        };
        let for_a_window = reading.get("tab").is_some_and(|tab| !tab.is_null());
        let has_rows = reading
            .get("rows")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|rows| !rows.is_empty());
        // A menu belongs to its window: one the reading holds for a window that is gone — closed
        // under it, or offering nothing any more — closes, as it does when its row is chosen,
        // rather than standing over the desk with nothing to say and holding the keyboard.
        if for_a_window && !has_rows {
            self.close_window_menu();
        }
        let up = for_a_window && has_rows;
        // The view drawing the slot is a program of its own, and one can end: a record of it that
        // outlives the process would leave the next menu up with nobody to draw it.
        if self.slots.running(VIEW).is_some() && !self.slot_view_runs(VIEW) {
            self.slots.forget(VIEW);
        }
        // The view is put up once, with the session, and shown or hidden as the reading opens and
        // closes the menu. One that ended is started again when a menu next opens, rather than on
        // every pass of a menu that is down: an app that will not stay up then costs one start an
        // opening, and not the session.
        if up || !self.slots.stood(VIEW) {
            let settings = self.read(mount::slots::BINDINGS);
            match self.slots.follow(VIEW, SHIPPED, settings.as_ref()) {
                Mount::Nothing => {}
                Mount::Stop => self.stop_slot_view(VIEW),
                Mount::Start { view, restart } => {
                    if restart {
                        // The person bound another view: the one that drew the slot goes before the
                        // new one takes it.
                        self.stop_slot_view(VIEW);
                    }
                    match view {
                        View::Program(program) => self.start_program_slot(VIEW, program, restart),
                        View::Package { dir, app, view } => {
                            self.start_package_slot(VIEW, dir, app, view, Some(&reading));
                        }
                    }
                }
            }
        }
        // What the view reads is what the session publishes for the slot now.
        if self.slot_views.running(VIEW) {
            self.slot_views.publish(VIEW, &reading);
        }
        // The view stands over the desk only while the reading says a menu is up: hidden, its
        // surface draws nothing, takes no input and keeps no room, and the opening that shows it
        // costs nothing.
        let namespace = surfaces::namespace(VIEW);
        if self.compositor.layer_shown(&namespace) != Some(up) {
            self.compositor.show_layer(&namespace, up);
        }
    }

    /// Start the program `program` that draws `slot` , over what lies under it.
    fn start_program_slot(&mut self, slot: &str, program: std::path::PathBuf, restart: bool) {
        if restart {
            self.surfaces.stop(slot);
        }
        let surface = surfaces::Surface::new(slot, program)
            .layer(surfaces::Layer::Overlay)
            .anchor(
                surfaces::Anchor::TOP
                    | surfaces::Anchor::BOTTOM
                    | surfaces::Anchor::LEFT
                    | surfaces::Anchor::RIGHT,
            )
            .keyboard(surfaces::Keyboard::OnDemand);
        if let Err(error) = self.start_surface(&surface) {
            tracing::warn!(%error, "the window menu view did not start");
            if let Some(tried) = self.slots.running(slot).cloned() {
                self.slots.failed(slot, tried);
            }
        }
    }

    /// Whether something draws `slot` now: the program the session started for it, or the package
    /// view running here.
    fn slot_view_runs(&self, slot: &str) -> bool {
        self.surfaces.running(slot) || self.slot_views.running(slot)
    }

    /// Close the window menu through the verb it publishes: the same path a press outside the menu
    /// and the person's own Escape take.
    fn close_window_menu(&mut self) {
        if let Err(error) = self.invoke(mount::shell::WINDOW_MENU, "close", &serde_json::Map::new())
        {
            tracing::warn!(%error, "a menu left up for a window that is gone was not closed");
        }
    }

    /// Run the view `view` of the tilolang package in `dir` for `slot` , over the reading the
    /// session publishes for it now. A package that is no view for the slot leaves it to the view
    /// the session ships with.
    fn start_package_slot(
        &mut self,
        slot: &'static str,
        dir: std::path::PathBuf,
        app: String,
        view: String,
        reading: Option<&serde_json::Value>,
    ) {
        let (Some(state), Some(reading)) = (mount::slots::surface(slot), reading) else {
            tracing::warn!(slot, "the slot has no state for a view of it to read");
            return;
        };
        let spec = tilo_surface_host::slot_view::Spec {
            app,
            view,
            dir,
            surface: state,
        };
        let waker = std::sync::Arc::new({
            let wake = self.compositor.waker();
            move || wake.wake_by_ref()
        });
        let person = self.shared.person.clone();
        let result = self.slot_views.start(
            slot,
            &spec,
            &person,
            &reading.to_string(),
            waker,
            &mut self.compositor,
        );
        match result {
            Ok(()) => {
                tracing::info!(slot, app = %spec.app, "a tilolang view draws the slot");
            }
            Err(why) => {
                tracing::warn!(slot, app = %spec.app, %why, "the view bound to the slot did not start");
                if let Some(tried) = self.slots.running(slot).cloned() {
                    self.slots.failed(slot, tried);
                }
            }
        }
    }

    /// Stop what draws `slot` , whichever kind of view it is.
    fn stop_slot_view(&mut self, slot: &str) {
        self.slot_views.stop(slot);
        self.surfaces.stop(slot);
    }

    /// A press away from what a slot's tilolang view drew closes the slot, as a press outside a
    /// menu does, and what else the view asked runs through the session's door.
    pub(crate) fn follow_slot_views(&mut self) -> Result<(), Error> {
        /// The one slot a person can bind a view to today.
        const VIEW: &str = mount::shell::WINDOW_MENU_SLOT;
        let Some(state) = mount::slots::surface(VIEW) else {
            return Ok(());
        };
        if self.slot_views.take_pressed_outside(VIEW) {
            // The slot's own verb, through the same door the view's presses go through.
            if let Err(error) = self.invoke(&state.id, "close", &serde_json::Map::new()) {
                tracing::warn!(%error, "a press outside the slot's view did not close it");
            }
            return Ok(());
        }
        if !self.slot_views.has_asks(VIEW) {
            return Ok(());
        }
        let doors = &self.shared.doors;
        self.slot_views.serve(VIEW, |ask| {
            let arguments: serde_json::Map<String, serde_json::Value> = ask
                .arguments
                .iter()
                .map(|(name, value)| (name.clone(), value.clone()))
                .collect();
            doors
                .invoke(
                    &ActionAddress::new(&ask.resource, &ask.action),
                    &arguments,
                    &mut tilo_surface_host::Look::none(),
                )
                .map(drop)
                .map_err(|error| error.to_string())
        });
        // What the view asked may have asked the session in turn.
        self.settle()?;
        Ok(())
    }

    /// Where the control the slot's view shows as `name` stands, logical `(x, y, w, h)` : what a
    /// test presses it by, and where the person's press on it lands.
    #[must_use]
    pub fn slot_control(&self, slot: &str, name: &str) -> Option<(f64, f64, f64, f64)> {
        self.slot_views.control(slot, name)
    }

    /// What the slot's view says, as assistive technology hears it: the labels of what it draws.
    #[must_use]
    pub fn slot_labels(&self, slot: &str) -> Vec<String> {
        self.slot_views.labels(slot)
    }
}
