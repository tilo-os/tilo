//! The desk widgets card: for each widget, whether it is on the desk and
//! when it shows itself. Each row is bound to its leaf of
//! [`crate::prefs::DeskWidgetsPrefs`], the section the compositor reads.
//! Where a widget rests and how wide its card is are set on the desk
//! itself, by dragging it and by its resize handle.

use provider_settings_ui::bound::SettingRows as _;
use settings_store::desk_widgets::{ShowMode, WidgetId, WidgetPrefs};
use ui::framework::ViewCx;
use ui::tilo_resource;
use ui::widget::Element;

use crate::i18n::t;
use crate::prefs::DeskWidgetsPrefs;
use provider_settings_ui::widgets::{Item, note, rows, section};

fn show_label(mode: ShowMode) -> String {
    t(match mode {
        ShowMode::Pause => "settings-desk-show-pause",
        ShowMode::Always => "settings-desk-show-always",
        ShowMode::Ask => "settings-desk-show-ask",
    })
}

/// Every desk widget (music, the people here, the calendar, the system
/// monitor): on the desk or not, when each shows. The one place widgets
/// are chosen; the top bar has none of its own. A widget's options show
/// while it is on.
pub(super) fn card(cx: &mut ViewCx<DeskWidgetsPrefs>) -> Element {
    let on = cx.value(|prefs: &DeskWidgetsPrefs| WidgetId::ALL.map(|id| widget(prefs, id).enabled));
    cx.switch(&on, |cx, on| {
        let mut body: Vec<Item> = Vec::new();
        for (id, on) in WidgetId::ALL.into_iter().zip(*on) {
            body.extend(widget_rows(cx, id, on));
        }
        body.push(note(&t("settings-desk-note")));
        section(&t("settings-desk-card"), rows(body)).into_element()
    })
}

/// The preferences of the widget `id`.
fn widget(prefs: &DeskWidgetsPrefs, id: WidgetId) -> &WidgetPrefs {
    match id {
        WidgetId::Music => &prefs.music,
        WidgetId::Team => &prefs.team,
        WidgetId::Calendar => &prefs.calendar,
        WidgetId::SystemMonitor => &prefs.system_monitor,
    }
}

/// The rows of the widget `id`: whether it is on the desk and, while it
/// is (`on`), when it shows.
fn widget_rows(cx: &ViewCx<DeskWidgetsPrefs>, id: WidgetId, on: bool) -> Vec<Item> {
    let paths = DeskWidgetsPrefs::paths();
    let (enabled, show): (
        tilo_resource::Path<DeskWidgetsPrefs, bool>,
        tilo_resource::Path<DeskWidgetsPrefs, ShowMode>,
    ) = match id {
        WidgetId::Music => (paths.music.enabled, paths.music.show),
        WidgetId::Team => (paths.team.enabled, paths.team.show),
        WidgetId::Calendar => (paths.calendar.enabled, paths.calendar.show),
        WidgetId::SystemMonitor => (paths.system_monitor.enabled, paths.system_monitor.show),
    };
    let (name, key) = match id {
        WidgetId::Music => (t("settings-desk-music"), "desk.music"),
        WidgetId::Team => (t("settings-desk-team"), "desk.team"),
        WidgetId::Calendar => (t("settings-desk-calendar"), "desk.calendar"),
        WidgetId::SystemMonitor => (t("settings-desk-system-monitor"), "desk.system-monitor"),
    };
    let mut items = vec![
        cx.setting(name)
            .id(format!("{key}.enabled"))
            .toggle(enabled),
    ];
    if on {
        items.push(
            cx.setting(t("settings-desk-when"))
                .id(format!("{key}.show"))
                .among(show, ShowMode::ALL, |mode: &ShowMode| show_label(*mode)),
        );
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A chooser tells its choices apart by their labels.
    #[test]
    fn every_offered_label_names_one_choice() {
        let shows: std::collections::HashSet<_> = ShowMode::ALL.map(show_label).into();
        assert_eq!(shows.len(), ShowMode::ALL.len());
    }
}
