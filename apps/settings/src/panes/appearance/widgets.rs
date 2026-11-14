//! The widgets card: for each widget, whether it is on the shell and when
//! it shows itself. Each row is bound to its leaf of
//! [`crate::prefs::WidgetsPrefs`], the section the compositor reads.

use provider_settings_ui::bound::SettingRows as _;
use settings_store::widgets::{ShowMode, WidgetId, WidgetPrefs};
use ui::framework::ViewCx;
use ui::tilo_resource;
use ui::widget::Element;

use crate::i18n::t;
use crate::prefs::WidgetsPrefs;
use provider_settings_ui::widgets::{Item, rows, section};

fn show_label(mode: ShowMode) -> String {
    t(match mode {
        ShowMode::Pause => "settings-widgets-show-pause",
        ShowMode::Always => "settings-widgets-show-always",
        ShowMode::Ask => "settings-widgets-show-ask",
    })
}

/// Every widget (music, the people here, the calendar, the system
/// monitor): on the shell or not, when each shows. The one place widgets
/// are chosen; the top bar has none of its own. A widget's options show
/// while it is on.
pub(super) fn card(cx: &mut ViewCx<WidgetsPrefs>) -> Element {
    let on = cx.value(|prefs: &WidgetsPrefs| WidgetId::ALL.map(|id| widget(prefs, id).enabled));
    cx.switch(&on, |cx, on| {
        let mut body: Vec<Item> = Vec::new();
        for (id, on) in WidgetId::ALL.into_iter().zip(*on) {
            body.extend(widget_rows(cx, id, on));
        }
        section(&t("settings-widgets-card"), rows(body)).into_element()
    })
}

fn widget(prefs: &WidgetsPrefs, id: WidgetId) -> &WidgetPrefs {
    match id {
        WidgetId::Music => &prefs.music,
        WidgetId::Team => &prefs.team,
        WidgetId::Calendar => &prefs.calendar,
        WidgetId::SystemMonitor => &prefs.system_monitor,
    }
}

fn widget_rows(cx: &ViewCx<WidgetsPrefs>, id: WidgetId, on: bool) -> Vec<Item> {
    let paths = WidgetsPrefs::paths();
    let (enabled, show): (
        tilo_resource::Path<WidgetsPrefs, bool>,
        tilo_resource::Path<WidgetsPrefs, ShowMode>,
    ) = match id {
        WidgetId::Music => (paths.music.enabled, paths.music.show),
        WidgetId::Team => (paths.team.enabled, paths.team.show),
        WidgetId::Calendar => (paths.calendar.enabled, paths.calendar.show),
        WidgetId::SystemMonitor => (paths.system_monitor.enabled, paths.system_monitor.show),
    };
    let (name, key) = match id {
        WidgetId::Music => (t("settings-widgets-music"), "shell.music"),
        WidgetId::Team => (t("settings-widgets-team"), "shell.team"),
        WidgetId::Calendar => (t("settings-widgets-calendar"), "shell.calendar"),
        WidgetId::SystemMonitor => (t("settings-widgets-system-monitor"), "shell.system-monitor"),
    };
    let mut items = vec![
        cx.setting(name)
            .id(format!("{key}.enabled"))
            .toggle(enabled),
    ];
    if on {
        items.push(
            cx.setting(t("settings-widgets-when"))
                .id(format!("{key}.show"))
                .among(show, ShowMode::ALL, |mode: &ShowMode| show_label(*mode)),
        );
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_offered_label_names_one_choice() {
        let shows: std::collections::HashSet<_> = ShowMode::ALL.map(show_label).into();
        assert_eq!(shows.len(), ShowMode::ALL.len());
    }
}
