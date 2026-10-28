//! Tube's controls, as slots in the window's bar ([`ui::window_toolbar`]):
//! the shell's sidebar toggle and Back, the screen's name as the location,
//! and Clear history on the History screen. The bar's search is Tube's
//! search; the window draws no header row.

use ui::widget::NavShell;
use ui::window_toolbar::{ToolbarAction, ToolbarEvent, ToolbarLocation, WindowToolbar};

use super::TubeApp;
use crate::action::{self, Action};
use crate::actions::TubeEffect;
use crate::i18n::t;
use crate::model::Route;
use crate::screens;

/// The bar action that clears the watch history.
pub(crate) const CLEAR_HISTORY: &str = "tube.clear-history";

impl TubeApp {
    /// The window bar's slots for what the window shows now.
    pub(crate) fn window_toolbar(&self) -> WindowToolbar {
        let mut toolbar = self.shell.window_toolbar();
        let title = screens::title(&self.model.route);
        toolbar.location = (!title.is_empty()).then(|| ToolbarLocation {
            label: title,
            ..ToolbarLocation::default()
        });
        if matches!(self.model.route, Route::History) && !self.model.history.is_empty() {
            NavShell::push_window_actions(
                &mut toolbar,
                [ToolbarAction::new(CLEAR_HISTORY, "trash", t("tube-clear-history")).in_menu()],
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
            if let ToolbarEvent::Action { id } = event
                && id == CLEAR_HISTORY
            {
                action::push(&self.bus, Action::Effect(TubeEffect::ClearHistory));
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
        let mut app = TubeApp::new();
        assert_eq!(location(&app).as_deref(), Some("Home"));
        assert!(action_names(&app).is_empty(), "{:?}", action_names(&app));
        app.apply(Action::Navigate(Route::Search("donut".into())));
        assert_eq!(location(&app).as_deref(), Some("Search: donut"));
        app.apply(Action::OpenVideo("vid_donut".into()));
        assert_eq!(location(&app), None, "a video page titles itself");
    }

    #[test]
    fn clear_history_is_the_window_bars_on_a_history_with_videos() {
        let mut app = TubeApp::new();
        app.apply(Action::OpenVideo("vid_donut".into()));
        app.apply(Action::Navigate(Route::History));
        let clear = app
            .window_toolbar()
            .actions
            .into_iter()
            .find(|action| action.id == CLEAR_HISTORY)
            .expect("the History screen offers Clear history in the bar");
        assert_eq!(clear.name, "Clear history");
        assert!(clear.menu, "a destructive action waits in the bar's menu");

        action::drain(&app.bus);
        ui::window_toolbar::deliver(ToolbarEvent::Action {
            id: CLEAR_HISTORY.into(),
        });
        assert!(app.take_window_toolbar_events());
        assert_eq!(
            action::drain(&app.bus),
            [Action::Effect(TubeEffect::ClearHistory)],
            "the bar's press asks for the same effect the palette runs"
        );

        app.perform(TubeEffect::ClearHistory).unwrap();
        assert!(
            !action_names(&app).contains(&"Clear history".to_owned()),
            "an empty history offers nothing to clear"
        );
    }
}
