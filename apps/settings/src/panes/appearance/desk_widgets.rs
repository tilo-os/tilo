//! The desk widgets card: for each widget, whether it is on the desk, when
//! it shows itself and which edge it waits at. Each row is bound to its
//! leaf of [`crate::prefs::DeskWidgetsPrefs`], the section the compositor
//! reads.

use settings_store::desk_widgets::{DockEdge, ShowMode, WidgetId, WidgetPrefs};
use ui::LocalState;
use ui::tilo_resource;

use super::AppearancePane;
use crate::i18n::t;
use crate::prefs::DeskWidgetsPrefs;
use crate::widgets::{Item, SettingsSection, card, note, rows};

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

fn show_from_label(label: &str) -> Result<ShowMode, String> {
    ShowMode::ALL
        .into_iter()
        .find(|mode| show_label(*mode) == label)
        .ok_or_else(|| format!("`{label}` is not offered"))
}

fn edge_from_label(label: &str) -> Result<DockEdge, String> {
    DockEdge::ALL
        .into_iter()
        .find(|edge| edge_label(*edge) == label)
        .ok_or_else(|| format!("`{label}` is not offered"))
}

impl AppearancePane {
    /// Music and Team: on the desk or not, when each shows, which edge.
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
            };
            body.extend(self.widget_rows(id, widget, (enabled, show, edge)));
        }
        body.push(note(&t("settings-desk-note")));
        card(&t("settings-desk-card"), rows(body))
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
        };
        let mut items = vec![
            self.cx
                .row(name)
                .id(format!("{key}.enabled"))
                .toggle(&LocalState::new(widget.enabled), enabled),
        ];
        if widget.enabled {
            items.push(
                self.cx
                    .row(t("settings-desk-when"))
                    .id(format!("{key}.show"))
                    .choice(
                        ShowMode::ALL.into_iter().map(show_label),
                        &LocalState::new(show_label(widget.show)),
                        show,
                        |label: String| show_from_label(&label),
                    ),
            );
            items.push(
                self.cx
                    .row(t("settings-desk-edge"))
                    .id(format!("{key}.edge"))
                    .choice(
                        DockEdge::ALL.into_iter().map(edge_label),
                        &LocalState::new(edge_label(widget.edge)),
                        edge,
                        |label: String| edge_from_label(&label),
                    ),
            );
        }
        items
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_offered_label_reads_back_as_its_choice() {
        for mode in ShowMode::ALL {
            assert_eq!(show_from_label(&show_label(mode)), Ok(mode));
        }
        for edge in DockEdge::ALL {
            assert_eq!(edge_from_label(&edge_label(edge)), Ok(edge));
        }
        assert!(show_from_label("Sometimes").is_err());
    }
}
