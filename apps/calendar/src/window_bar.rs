//! The calendar's toolbar, as slots in the window's bar
//! ([`ui::window_toolbar`]).
//!
//! The bar shows the sidebar's toggle, previous, next and Today, the period
//! as the window's location, the views (Month, Week, Day, Agenda, Tasks) as
//! its switcher and New event (New task on Tasks); exporting and importing
//! the calendar file are in its menu. Search is the bar's own.

use chrono::NaiveDate;
use ui::format::Period;
use ui::framework::BarCommands;
use ui::window_toolbar::{ToolbarLocation, ToolbarView, ToolbarViews, WindowToolbar};

use crate::{date, model::View};

/// What the person asked of the calendar in the window's bar.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum BarAction {
    Previous,
    Next,
    Today,
    View(View),
    New,
    QuickAdd,
    Shortcuts,
    Export,
    Import,
}

/// The period `view` shows around `anchor` (none: Tasks, which shows no
/// span of days).
pub(crate) fn period(view: View, anchor: NaiveDate) -> Option<Period> {
    match view {
        View::Tasks => None,
        View::Month => Some(Period::Month(anchor)),
        View::Week => {
            let week = date::week_days(anchor);
            let first = week.first().copied().unwrap_or(anchor);
            let last = week.last().copied().unwrap_or(anchor);
            Some(Period::Days(first, last))
        }
        View::Day => Some(Period::Day(anchor)),
        View::Agenda => Some(Period::Days(anchor, crate::agenda::last_day(anchor))),
    }
}

/// The window's location for `view` around `anchor`: its period, in the
/// widest form the bar has room for, or on Tasks the list shown
/// (`tasks_list`).
fn location(view: View, anchor: NaiveDate, tasks_list: &str) -> ToolbarLocation {
    period(view, anchor).map_or_else(
        || ToolbarLocation {
            label: tasks_list.to_owned(),
            ..ToolbarLocation::default()
        },
        ToolbarLocation::period,
    )
}

/// The window bar's slots for `view` around `anchor` (on Tasks the list
/// `tasks_list`), the sidebar shown or not.
pub(crate) fn window_toolbar(
    bar: &BarCommands<BarAction>,
    view: View,
    anchor: NaiveDate,
    tasks_list: &str,
    sidebar_open: bool,
) -> WindowToolbar {
    let mut toolbar = WindowToolbar {
        sidebar: Some(sidebar_open),
        location: Some(location(view, anchor, tasks_list)),
        views: Some(ToolbarViews {
            name: "View".into(),
            choices: View::ALL
                .iter()
                .map(|choice| ToolbarView {
                    id: bar.entry("", "", BarAction::View(*choice)).id,
                    label: choice.display_label(),
                })
                .collect(),
            chosen: bar.entry("", "", BarAction::View(view)).id,
        }),
        ..WindowToolbar::default()
    };
    let period = match view {
        View::Month => Some(("Previous month", "Next month")),
        View::Week | View::Agenda => Some(("Previous week", "Next week")),
        View::Day => Some(("Previous day", "Next day")),
        View::Tasks => None,
    };
    if let Some((previous, next)) = period {
        toolbar.leading = vec![
            bar.entry("chevron-left", previous, BarAction::Previous)
                .navigating(),
            bar.entry("chevron-right", next, BarAction::Next)
                .navigating(),
            bar.entry("calendar", "Today", BarAction::Today).labelled(),
        ];
    }
    let new = if view == View::Tasks {
        crate::resource::t("calendar-tasks-new")
    } else {
        "New event".to_owned()
    };
    toolbar.actions = vec![
        bar.entry("plus", new, BarAction::New),
        // The one-line add sits beside the full form it saves a person from.
        bar.entry(
            "zap",
            crate::resource::t("calendar-quick-title"),
            BarAction::QuickAdd,
        )
        .in_menu(),
        bar.entry(
            "keyboard",
            crate::resource::t("calendar-help-title"),
            BarAction::Shortcuts,
        )
        .in_menu(),
        bar.entry("upload", "Export calendar", BarAction::Export)
            .in_menu(),
        bar.entry("download", "Import calendar", BarAction::Import)
            .in_menu(),
    ];
    toolbar
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Datelike;
    use ui::window_toolbar::ToolbarEvent;

    fn anchor() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 10, 21).unwrap_or_default()
    }

    #[test]
    fn the_calendars_controls_are_the_window_bars_slots() {
        let bar = BarCommands::new();
        let toolbar = window_toolbar(&bar, View::Week, anchor(), "Inbox", true);
        assert_eq!(toolbar.sidebar, Some(true));
        let leading: Vec<&str> = toolbar.leading.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(leading, ["Previous week", "Next week", "Today"]);
        let views = toolbar.views.as_ref().expect("the views switch in the bar");
        assert_eq!(views.choices.len(), View::ALL.len());
        let pressed = |id: &str| bar.pressed(&ToolbarEvent::Action { id: id.into() });
        assert_eq!(pressed(&views.chosen), Some(BarAction::View(View::Week)));
        assert_eq!(
            toolbar.actions.first().map(|a| a.name.as_str()),
            Some("New event")
        );
        assert!(
            toolbar.actions.iter().skip(1).all(|a| a.menu),
            "the file commands are the menu's"
        );
        assert!(
            toolbar.location.as_ref().is_some_and(|location| location
                .label
                .contains(&anchor().year().to_string())
                || !location.label.is_empty()),
            "the period is the window's location"
        );

        let tasks = window_toolbar(&bar, View::Tasks, anchor(), "Garden", false);
        assert!(
            tasks.leading.is_empty(),
            "Tasks has no period to step through"
        );
        assert_eq!(
            tasks.location.map(|location| location.label).as_deref(),
            Some("Garden"),
            "on Tasks the location names the list shown"
        );

        for (choice, view) in views.choices.iter().zip(View::ALL) {
            assert_eq!(
                pressed(&choice.id),
                Some(BarAction::View(view)),
                "{} chooses its view",
                choice.label
            );
        }
    }
}
