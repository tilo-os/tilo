//! What assistive technology addresses on the map: the controls and rows the last paint recorded, under stable keys.

use super::*;

impl MapsApp {
    /// Immediate-mode controls exported as one semantic list. Keeping this
    /// derived from the same hit targets used by pointer routing prevents the
    /// accessibility tree from advertising controls that are not on screen.
    #[allow(
        clippy::too_many_lines,
        reason = "the exhaustive mapping keeps each painted Maps command next to its stable semantic id, role, and derived state"
    )]
    pub(super) fn accessibility_target_for_hit(&self, hit: &Hit) -> Option<AccessibilityTarget> {
        use ui::a11y::{AccessState, Role};

        let (label, target, role, state) = match hit {
            Hit::Consume | Hit::CloseNav => return None,
            Hit::OpenSearch => (
                "Search places".to_string(),
                "maps.search".to_string(),
                Role::Button,
                AccessState::new().focusable(true),
            ),
            Hit::OpenEmptySearch => (
                "Search places".to_string(),
                "maps.empty-search".to_string(),
                Role::Button,
                AccessState::new().focusable(true),
            ),
            Hit::ToggleCollapse => (
                if self.panel_collapsed {
                    "Expand panel"
                } else {
                    "Collapse panel"
                }
                .to_string(),
                "maps.panel-collapse".to_string(),
                Role::ToggleButton,
                AccessState::new()
                    .focusable(true)
                    .checked(self.panel_collapsed),
            ),
            Hit::PanelBack => (
                "Back to places".to_string(),
                "maps.panel.back".to_string(),
                Role::Button,
                AccessState::new().focusable(true),
            ),
            Hit::Mode(mode) => (
                format!("Travel by {}", mode.label()),
                format!("maps.mode:{}", mode.label().to_lowercase()),
                Role::RadioButton,
                AccessState::new()
                    .focusable(true)
                    .checked(self.model.mode == *mode),
            ),
            Hit::SelectPlace(id) => (
                self.model.place(id).map_or_else(
                    || "Open place".to_string(),
                    |place| format!("Open {}", place.name),
                ),
                format!("maps.place:{id}"),
                Role::Button,
                AccessState::new().focusable(true),
            ),
            Hit::SelectPoi(id) => (
                self.pois.iter().find(|poi| poi.id == *id).map_or_else(
                    || "Open nearby place".to_string(),
                    |poi| format!("Open {}", poi.name),
                ),
                format!("maps.poi:{id}"),
                Role::Button,
                AccessState::new().focusable(true),
            ),
            Hit::ClosePoi => (
                "Close place details".to_string(),
                "maps.poi.close".to_string(),
                Role::Button,
                AccessState::new().focusable(true),
            ),
            Hit::PoiDirections => (
                "Directions to this place".to_string(),
                "maps.poi.directions".to_string(),
                Role::Button,
                AccessState::new().focusable(true),
            ),
            Hit::Recent(query) => (
                format!("Search again for {query}"),
                format!("maps.recent:{query}"),
                Role::Button,
                AccessState::new().focusable(true),
            ),
            Hit::ClearRecents => (
                "Clear search history".to_string(),
                "maps.recent.clear".to_string(),
                Role::Button,
                AccessState::new().focusable(true),
            ),
            Hit::ToggleNav => (
                if self.nav_open {
                    "Hide places"
                } else {
                    "Show places"
                }
                .to_string(),
                "maps.navigation".to_string(),
                Role::ToggleButton,
                AccessState::new().focusable(true).checked(self.nav_open),
            ),
            Hit::Directions => (
                "Directions".to_string(),
                "maps.directions".to_string(),
                Role::Button,
                AccessState::new().focusable(true),
            ),
            Hit::Transit => (
                "Transit".to_string(),
                "maps.transit".to_string(),
                Role::Button,
                AccessState::new().focusable(true),
            ),
            Hit::ToggleSave => (
                "Save or unsave place".to_string(),
                "maps.save".to_string(),
                Role::ToggleButton,
                AccessState::new()
                    .focusable(true)
                    .checked(self.model.selected_place().is_some_and(|place| place.saved)),
            ),
            Hit::ClearRoute => (
                "Clear route".to_string(),
                "maps.route.clear".to_string(),
                Role::Button,
                AccessState::new().focusable(true),
            ),
            Hit::NextRoute => (
                "Another route".to_string(),
                "maps.route.another".to_string(),
                Role::Button,
                AccessState::new().focusable(true),
            ),
            Hit::UseMyLocation => (
                "Use my location".to_string(),
                "maps.location".to_string(),
                Role::Button,
                AccessState::new().focusable(true),
            ),
            Hit::TransitTimeMode(TransitTimeMode::Depart) => (
                "Leave at".to_string(),
                "maps.transit-time:depart".to_string(),
                Role::RadioButton,
                AccessState::new()
                    .focusable(true)
                    .checked(self.model.transit_time.mode == TransitTimeMode::Depart),
            ),
            Hit::TransitTimeMode(TransitTimeMode::Arrive) => (
                "Arrive by".to_string(),
                "maps.transit-time:arrive".to_string(),
                Role::RadioButton,
                AccessState::new()
                    .focusable(true)
                    .checked(self.model.transit_time.mode == TransitTimeMode::Arrive),
            ),
            Hit::TransitTimeDelta(minutes) => (
                if *minutes < 0 { "Earlier" } else { "Later" }.to_string(),
                format!("maps.transit-time:delta:{minutes}"),
                Role::Button,
                AccessState::new().focusable(true),
            ),
            Hit::TransitTimeNow => (
                "Reset to now".to_string(),
                "maps.transit-time:now".to_string(),
                Role::Button,
                AccessState::new().focusable(true),
            ),
            Hit::Journey(index) => (
                format!("Journey option {}", index + 1),
                format!("maps.journey:{index}"),
                Role::RadioButton,
                AccessState::new()
                    .focusable(true)
                    .checked(self.model.transit_choice == *index),
            ),
            Hit::ToggleOverflow => (
                "More map options".to_string(),
                "maps.overflow".to_string(),
                Role::ToggleButton,
                AccessState::new()
                    .focusable(true)
                    .checked(self.menu_open(MapMenu::More)),
            ),
        };
        Some(AccessibilityTarget {
            rect: Rect::ZERO,
            label,
            target,
            role,
            state,
        })
    }

    pub(super) fn accessibility_targets(&self) -> Vec<AccessibilityTarget> {
        use crate::map_controls::MapControlAction;
        use ui::a11y::{AccessState, Role};

        let mut targets = vec![
            AccessibilityTarget {
                rect: self.map_controls.rect(MapControlAction::History),
                label: "History".to_string(),
                target: ui::widget::HISTORY_CONTROL_ID.to_string(),
                role: Role::ToggleButton,
                state: AccessState::new().focusable(true).checked(
                    self.history
                        .control()
                        .is_some_and(ui::widget::HistoryControl::is_open),
                ),
            },
            AccessibilityTarget {
                rect: self.map_controls.rect(MapControlAction::ZoomIn),
                label: "Zoom in".to_string(),
                target: "maps.control:zoom-in".to_string(),
                role: Role::Button,
                state: AccessState::new().focusable(true),
            },
            AccessibilityTarget {
                rect: self.map_controls.rect(MapControlAction::ZoomOut),
                label: "Zoom out".to_string(),
                target: "maps.control:zoom-out".to_string(),
                role: Role::Button,
                state: AccessState::new().focusable(true),
            },
            AccessibilityTarget {
                rect: self.map_controls.rect(MapControlAction::Locate),
                label: "Use my location".to_string(),
                target: "maps.control:locate".to_string(),
                role: Role::Button,
                state: AccessState::new().focusable(true),
            },
            AccessibilityTarget {
                rect: self.map_controls.rect(MapControlAction::Layers),
                label: "Map layers".to_string(),
                target: "maps.control:layers".to_string(),
                role: Role::ToggleButton,
                state: AccessState::new()
                    .focusable(true)
                    .checked(self.menu_open(MapMenu::Layers)),
            },
        ];
        targets.retain(|target| target.rect.area() > 0.0);
        if self.navigation_end_rect.area() > 0.0 && self.model.route.is_some() {
            targets.push(AccessibilityTarget {
                rect: self.navigation_end_rect,
                label: "End navigation".to_string(),
                target: "maps.navigation.end".to_string(),
                role: Role::Button,
                state: AccessState::new().focusable(true),
            });
        }
        targets.extend(self.click_targets.iter().filter_map(|(rect, hit)| {
            self.accessibility_target_for_hit(hit)
                .and_then(|mut target| {
                    (rect.area() > 0.0).then(|| {
                        target.rect = *rect;
                        target
                    })
                })
        }));
        if self.overlay_mode() && self.nav_open {
            let panel = self.panel_rect();
            targets.retain(|target| {
                matches!(target.target.as_str(), "maps.navigation" | "maps.search")
                    || panel.contains(target.rect.center())
            });
        }
        targets
    }
}
