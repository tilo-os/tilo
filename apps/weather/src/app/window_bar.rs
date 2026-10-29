//! Weather's controls, as slots in the window's bar ([`ui::window_toolbar`]):
//! Back where the window shows one column at a time, the place shown as the
//! location, Add place, and Refresh while a forecast shows.

use ui::widget::{NavPane, NavShell};
use ui::window_toolbar::{ToolbarLocation, WindowToolbar};

use super::WeatherApp;

/// What the bar's entries do.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum Command {
    /// Show the places and start a search for one to add.
    AddPlace,
    /// Fetch the forecast shown again.
    Refresh,
}

impl WeatherApp {
    /// The window bar's slots for what the window shows now.
    pub(crate) fn window_toolbar(&self) -> WindowToolbar {
        let mut toolbar = self.nav.window_toolbar();
        let core = self.core.borrow();
        let places_in_front =
            !self.nav.geometry().side_by_side && self.nav.front_pane() == NavPane::Overview;
        // The places are where the window is while they are in front or
        // while no place is chosen yet.
        let label = core
            .model
            .selected_location()
            .filter(|_| !places_in_front)
            .map_or_else(|| "Places".to_owned(), |location| location.name.clone());
        toolbar.location = Some(ToolbarLocation {
            label,
            ..ToolbarLocation::default()
        });
        let mut actions = vec![self.bar.entry("plus", "Add place", Command::AddPlace)];
        if Self::shows_forecast(&core) {
            actions.push(
                self.bar
                    .entry("refresh", "Refresh forecast", Command::Refresh),
            );
        }
        NavShell::push_window_actions(&mut toolbar, actions);
        toolbar
    }

    /// Run what the person did in the window's bar: the shell's own slots,
    /// then Weather's. Returns whether anything was asked.
    pub(super) fn take_window_toolbar_events(&mut self) -> bool {
        let mut asked = false;
        for event in ui::window_toolbar::take_events() {
            asked = true;
            if self.nav.window_toolbar_event(&event) {
                continue;
            }
            match self.bar.pressed(&event) {
                Some(Command::AddPlace) => {
                    if !self.nav.geometry().side_by_side {
                        self.nav.show(NavPane::Overview);
                    }
                    self.core.borrow_mut().begin_search();
                }
                Some(Command::Refresh) => self.core.borrow_mut().refresh_selected_forecast(),
                None => {}
            }
        }
        asked
    }

    pub(super) fn declare_window_toolbar(&self) {
        ui::window_toolbar::declare(Some(self.window_toolbar()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::headless::Framed;
    use ui::theme::Theme;

    fn window(w: f64, h: f64) -> Framed<WeatherApp> {
        let mut framed = Framed::new(
            ui::State::new(),
            WeatherApp::sample(),
            ui::headless::viewport((w, h)),
            Theme::dark(),
        );
        Framed::harness_mut(&mut framed).relayout();
        framed
    }

    fn action_names(app: &WeatherApp) -> Vec<String> {
        app.window_toolbar()
            .actions
            .into_iter()
            .map(|action| action.name)
            .collect()
    }

    #[test]
    fn the_window_bar_names_the_place_and_holds_its_actions() {
        let framed = window(1280.0, 720.0);
        let place = framed
            .core
            .borrow()
            .model
            .selected_location()
            .map(|location| location.name.clone());
        assert!(place.is_some(), "the sample selects a place");
        assert_eq!(
            framed
                .window_toolbar()
                .location
                .map(|location| location.label),
            place
        );
        assert_eq!(action_names(&framed), ["Add place", "Refresh forecast"]);
        assert_eq!(
            ui::window_toolbar::declared(),
            Some(framed.window_toolbar())
        );
    }

    #[test]
    fn one_column_at_a_time_the_bar_names_the_places_and_add_place_brings_them_back() {
        let mut framed = window(420.0, 720.0);
        assert_eq!(framed.nav.front_pane(), NavPane::Overview);
        assert_eq!(
            framed
                .window_toolbar()
                .location
                .map(|location| location.label)
                .as_deref(),
            Some("Places")
        );

        framed.nav.show(NavPane::Content);
        framed.nav.snap();
        Framed::harness_mut(&mut framed).relayout();
        Framed::harness_mut(&mut framed)
            .press_window_action("Add place")
            .unwrap();
        assert_eq!(framed.nav.front_pane(), NavPane::Overview);
        assert!(
            framed.core.borrow().search_is_focused(),
            "Add place shows the places and focuses the field"
        );
    }
}
