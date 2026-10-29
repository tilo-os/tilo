//! Tube's controls, as slots in the window's bar ([`ui::window_toolbar`]):
//! the shell's sidebar toggle and Back, the screen's name as the location,
//! and Clear history on the History screen. The bar's search is Tube's
//! search; the window draws no header row.

use ui::widget::NavShell;
use ui::window_toolbar::{ToolbarLocation, WindowToolbar};

use super::TubeApp;
use crate::action::{self, Action};
use crate::actions::TubeEffect;
use crate::i18n::t;
use crate::model::Route;
use crate::screens;

impl TubeApp {
    /// The window bar's slots for what the window shows now.
    pub(crate) fn window_toolbar(&self) -> WindowToolbar {
        let mut toolbar = self.shell.window_toolbar();
        let tube = self.tube.borrow();
        let title = screens::title(&tube.model.route);
        toolbar.location = (!title.is_empty()).then(|| ToolbarLocation {
            label: title,
            ..ToolbarLocation::default()
        });
        if matches!(tube.model.route, Route::History) && !tube.model.history.is_empty() {
            NavShell::push_window_actions(
                &mut toolbar,
                [tube
                    .bar
                    .entry(
                        "trash",
                        t("tube-clear-history"),
                        Action::Effect(TubeEffect::ClearHistory),
                    )
                    .in_menu()],
            );
        }
        toolbar
    }

    /// Run what the person did in the window's bar: the shell's own slots,
    /// then Tube's. Returns whether anything was asked of Tube.
    pub(super) fn take_window_toolbar_events(&mut self) -> bool {
        let mut asked = false;
        for event in ui::window_toolbar::take_events() {
            asked = true;
            if self.shell.window_toolbar_event(&event) {
                continue;
            }
            let tube = self.tube.borrow();
            if let Some(action) = tube.bar.pressed(&event) {
                action::push(&tube.bus, action);
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
    use ui::window_toolbar::ToolbarEvent;

    fn location(app: &TubeApp) -> Option<String> {
        app.window_toolbar().location.map(|location| location.label)
    }

    fn action_names(app: &TubeApp) -> Vec<String> {
        app.window_toolbar()
            .actions
            .into_iter()
            .map(|action| action.name)
            .collect()
    }

    #[test]
    fn the_window_bar_names_the_screen_and_the_page_titles_itself() {
        let app = TubeApp::sample();
        assert_eq!(location(&app).as_deref(), Some("Home"));
        assert!(action_names(&app).is_empty(), "{:?}", action_names(&app));
        app.tube()
            .apply(Action::Navigate(Route::Search("donut".into())));
        assert_eq!(location(&app).as_deref(), Some("Search: donut"));
        app.tube().apply(Action::OpenVideo("vid_donut".into()));
        assert_eq!(location(&app), None, "a video page titles itself");
    }

    #[test]
    fn clear_history_is_the_window_bars_on_a_history_with_videos() {
        let mut app = TubeApp::sample();
        app.tube().apply(Action::OpenVideo("vid_donut".into()));
        app.tube().apply(Action::Navigate(Route::History));
        let clear = app
            .window_toolbar()
            .actions
            .into_iter()
            .find(|action| action.name == "Clear history")
            .expect("the History screen offers Clear history in the bar");
        assert!(clear.menu, "a destructive action waits in the bar's menu");

        action::drain(&app.tube().bus);
        ui::window_toolbar::deliver(ToolbarEvent::Action { id: clear.id });
        assert!(app.take_window_toolbar_events());
        assert_eq!(
            action::drain(&app.tube().bus),
            [Action::Effect(TubeEffect::ClearHistory)],
            "the bar's press asks for the same effect the palette runs"
        );

        app.tube().perform(TubeEffect::ClearHistory).unwrap();
        assert!(
            !action_names(&app).contains(&"Clear history".to_owned()),
            "an empty history offers nothing to clear"
        );
    }
}
