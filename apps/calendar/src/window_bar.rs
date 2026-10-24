//! The calendar's toolbar, as slots in the window's bar
//! ([`ui::window_toolbar`]).
//!
//! The bar shows the sidebar's toggle, previous, next and Today, the period
//! as the window's location, the views (Month, Week, Day, Agenda, Tasks) as
//! its switcher and New event (New task on Tasks); exporting and importing
//! the calendar file are in its menu. Search is the bar's own.

use chrono::NaiveDate;
use ui::format::Period;
use ui::window_toolbar::{
    ToolbarAction, ToolbarEvent, ToolbarLocation, ToolbarView, ToolbarViews, WindowToolbar,
};

use crate::{date, model::View};

const PREVIOUS: &str = "previous";
const NEXT: &str = "next";
const TODAY: &str = "today";
/// New event (New task on Tasks).
pub(crate) const NEW: &str = "new";
const EXPORT: &str = "export";
const IMPORT: &str = "import";
/// A view's slot id: `view:` and its label.
const VIEW_PREFIX: &str = "view:";

/// The slot id of `view` in the bar's switcher.
pub(crate) fn view_slot(view: View) -> String {
    format!("{VIEW_PREFIX}{}", view.label())
}

/// What the person asked of the calendar in the window's bar.
pub(crate) enum BarAction {
    ToggleSidebar,
    Previous,
    Next,
    Today,
    View(View),
    New,
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
/// widest form the bar has room for, or the task list's title.
fn location(view: View, anchor: NaiveDate) -> ToolbarLocation {
    period(view, anchor).map_or_else(
        || ToolbarLocation {
            label: crate::resource::t("calendar-tasks-title"),
            ..ToolbarLocation::default()
        },
        ToolbarLocation::period,
    )
}

/// The window bar's slots for `view` around `anchor`, the sidebar shown or
/// not.
pub(crate) fn window_toolbar(view: View, anchor: NaiveDate, sidebar_open: bool) -> WindowToolbar {
    let mut toolbar = WindowToolbar {
        sidebar: Some(sidebar_open),
        location: Some(location(view, anchor)),
        views: Some(ToolbarViews {
            name: "View".into(),
            choices: View::ALL
                .iter()
                .map(|choice| ToolbarView {
                    id: view_slot(*choice),
                    label: choice.display_label(),
                })
                .collect(),
            chosen: view_slot(view),
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
            ToolbarAction::new(PREVIOUS, "chevron-left", previous).navigating(),
            ToolbarAction::new(NEXT, "chevron-right", next).navigating(),
            ToolbarAction::new(TODAY, "calendar", "Today").labelled(),
        ];
    }
    let new = if view == View::Tasks {
        crate::resource::t("calendar-tasks-new")
    } else {
        "New event".to_owned()
    };
    toolbar.actions = vec![
        ToolbarAction::new(NEW, "plus", new),
        ToolbarAction::new(EXPORT, "upload", "Export calendar").in_menu(),
        ToolbarAction::new(IMPORT, "download", "Import calendar").in_menu(),
    ];
    toolbar
}

/// What `event` asks of the calendar, if it is one of its slots.
pub(crate) fn bar_action(event: &ToolbarEvent) -> Option<BarAction> {
    Some(match event {
        ToolbarEvent::ToggleSidebar => BarAction::ToggleSidebar,
        ToolbarEvent::Action { id } => match id.as_str() {
            PREVIOUS => BarAction::Previous,
            NEXT => BarAction::Next,
            TODAY => BarAction::Today,
            NEW => BarAction::New,
            EXPORT => BarAction::Export,
            IMPORT => BarAction::Import,
            other => {
                let label = other.strip_prefix(VIEW_PREFIX)?;
                BarAction::View(*View::ALL.iter().find(|view| view.label() == label)?)
            }
        },
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Datelike;

    fn anchor() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 10, 21).unwrap_or_default()
    }

    #[test]
    fn the_calendars_controls_are_the_window_bars_slots() {
        let toolbar = window_toolbar(View::Week, anchor(), true);
        assert_eq!(toolbar.sidebar, Some(true));
        let leading: Vec<&str> = toolbar.leading.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(leading, [PREVIOUS, NEXT, TODAY]);
        let views = toolbar.views.as_ref().expect("the views switch in the bar");
        assert_eq!(views.choices.len(), View::ALL.len());
        assert_eq!(views.chosen, format!("{VIEW_PREFIX}{}", View::Week.label()));
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

        let tasks = window_toolbar(View::Tasks, anchor(), false);
        assert!(
            tasks.leading.is_empty(),
            "Tasks has no period to step through"
        );

        for choice in &views.choices {
            assert!(
                matches!(
                    bar_action(&ToolbarEvent::Action {
                        id: choice.id.clone()
                    }),
                    Some(BarAction::View(_))
                ),
                "{} chooses its view",
                choice.id
            );
        }
    }
}
