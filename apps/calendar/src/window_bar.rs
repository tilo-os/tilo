//! The calendar's toolbar, as slots in the window's bar
//! ([`ui::window_toolbar`]).

use chrono::NaiveDate;
use ui::format::Period;
use ui::framework::BarCommands;
use ui::window_toolbar::{ToolbarLocation, ToolbarView, ToolbarViews, WindowToolbar};

use crate::{date, model::View};

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

fn location(view: View, anchor: NaiveDate, tasks_list: &str) -> ToolbarLocation {
    period(view, anchor).map_or_else(
        || ToolbarLocation {
            label: tasks_list.to_owned(),
            ..ToolbarLocation::default()
        },
        ToolbarLocation::period,
    )
}

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
            name: crate::resource::t("calendar-views"),
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
        View::Month => Some((
            crate::resource::t("calendar-previous-month"),
            crate::resource::t("calendar-next-month"),
        )),
        View::Week | View::Agenda => Some((
            crate::resource::t("calendar-help-previous-week"),
            crate::resource::t("calendar-help-next-week"),
        )),
        View::Day => Some((
            crate::resource::t("calendar-help-previous-day"),
            crate::resource::t("calendar-help-next-day"),
        )),
        View::Tasks => None,
    };
    if let Some((previous, next)) = period {
        toolbar.leading = vec![
            bar.entry("chevron-left", previous, BarAction::Previous)
                .navigating(),
            bar.entry("chevron-right", next, BarAction::Next)
                .navigating(),
            bar.entry(
                "calendar",
                crate::resource::t("calendar-tasks-today"),
                BarAction::Today,
            )
            .labelled(),
        ];
    }
    let new = if view == View::Tasks {
        crate::resource::t("calendar-tasks-new")
    } else {
        crate::resource::t("calendar-a11y-new-event")
    };
    toolbar.actions = vec![
        bar.entry("plus", new, BarAction::New),
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
        bar.entry(
            "upload",
            crate::resource::t("calendar-a11y-export"),
            BarAction::Export,
        )
        .in_menu(),
        bar.entry(
            "download",
            crate::resource::t("calendar-a11y-import"),
            BarAction::Import,
        )
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
