//! The desk widgets card: for each widget, whether it is on the desk, when
//! it shows itself and which edge it waits at. Each row is bound to its
//! leaf of [`crate::prefs::DeskWidgetsPrefs`], the section the compositor
//! reads.

use settings_store::desk_widgets::{DockEdge, ShowMode, WidgetId, WidgetPrefs};
use ui::tilo_resource;

use super::AppearancePane;
use crate::i18n::t;
use crate::prefs::DeskWidgetsPrefs;
use crate::widgets::{Item, SettingsSection, note, rows, section};

fn show_label(mode: ShowMode) -> String {
    t(match mode {
        ShowMode::Pause => "settings-desk-show-pause",
        ShowMode::Always => "settings-desk-show-always",
        ShowMode::Ask => "settings-desk-show-ask",
    })
}

fn edge_label(edge: DockEdge) -> String {
    t(match edge {
        DockEdge::Left => "settings-desk-edge-left",
        DockEdge::Right => "settings-desk-edge-right",
        DockEdge::Bottom => "settings-desk-edge-bottom",
        DockEdge::BottomLeft => "settings-desk-edge-bottom-left",
        DockEdge::BottomRight => "settings-desk-edge-bottom-right",
    })
}

impl AppearancePane {
    /// Every desk widget (music, the people here, the calendar, the system
    /// monitor): on the desk or not, when each shows, which edge. The one
    /// place widgets are chosen; the top bar has none of its own.
    pub(super) fn desk_widgets_card(&self) -> SettingsSection {
        let prefs = self.desk.get();
        let mut body: Vec<Item> = Vec::new();
        for id in WidgetId::ALL {
            let (widget, enabled, show, edge) = match id {
                WidgetId::Music => (
                    &prefs.music,
                    DeskWidgetsPrefs::paths().music.enabled,
                    DeskWidgetsPrefs::paths().music.show,
                    DeskWidgetsPrefs::paths().music.edge,
                ),
                WidgetId::Team => (
                    &prefs.team,
                    DeskWidgetsPrefs::paths().team.enabled,
                    DeskWidgetsPrefs::paths().team.show,
                    DeskWidgetsPrefs::paths().team.edge,
                ),
                WidgetId::Calendar => (
                    &prefs.calendar,
                    DeskWidgetsPrefs::paths().calendar.enabled,
                    DeskWidgetsPrefs::paths().calendar.show,
                    DeskWidgetsPrefs::paths().calendar.edge,
                ),
                WidgetId::SystemMonitor => (
                    &prefs.system_monitor,
                    DeskWidgetsPrefs::paths().system_monitor.enabled,
                    DeskWidgetsPrefs::paths().system_monitor.show,
                    DeskWidgetsPrefs::paths().system_monitor.edge,
                ),
            };
            body.extend(self.widget_rows(id, widget, (enabled, show, edge)));
        }
        body.push(note(&t("settings-desk-note")));
        section(&t("settings-desk-card"), rows(body))
    }

    fn widget_rows(
        &self,
        id: WidgetId,
        widget: &WidgetPrefs,
        (enabled, show, edge): (
            tilo_resource::Path<DeskWidgetsPrefs, bool>,
            tilo_resource::Path<DeskWidgetsPrefs, ShowMode>,
            tilo_resource::Path<DeskWidgetsPrefs, DockEdge>,
        ),
    ) -> Vec<Item> {
        let (name, key) = match id {
            WidgetId::Music => (t("settings-desk-music"), "desk.music"),
            WidgetId::Team => (t("settings-desk-team"), "desk.team"),
            WidgetId::Calendar => (t("settings-desk-calendar"), "desk.calendar"),
            WidgetId::SystemMonitor => (t("settings-desk-system-monitor"), "desk.system-monitor"),
        };
        let mut items = vec![
            self.cx
                .row(name)
                .id(format!("{key}.enabled"))
                .of(&self.desk, enabled)
                .toggle(),
        ];
        if widget.enabled {
            items.push(
                self.cx
                    .row(t("settings-desk-when"))
                    .id(format!("{key}.show"))
                    .of(&self.desk, show)
                    .among(ShowMode::ALL, |mode: &ShowMode| show_label(*mode)),
            );
            items.push(
                self.cx
                    .row(t("settings-desk-edge"))
                    .id(format!("{key}.edge"))
                    .of(&self.desk, edge)
                    .among(DockEdge::ALL, |edge: &DockEdge| edge_label(*edge)),
            );
        }
        items
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A chooser tells its choices apart by their labels.
    #[test]
    fn every_offered_label_names_one_choice() {
        let shows: std::collections::HashSet<_> = ShowMode::ALL.map(show_label).into();
        assert_eq!(shows.len(), ShowMode::ALL.len());
        let edges: std::collections::HashSet<_> = DockEdge::ALL.map(edge_label).into();
        assert_eq!(edges.len(), DockEdge::ALL.len());
    }
}
