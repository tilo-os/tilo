//! What assistive technology addresses on the map: the controls and rows the last paint recorded, under stable keys.

use super::{AccessibilityTarget, Hit, MapMenu, MapsApp, Rect, TransitTimeMode};
use crate::failure;

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
                failure::t("maps-search-places"),
                "maps.search".to_string(),
                Role::Button,
                AccessState::new().focusable(true),
            ),
            Hit::OpenEmptySearch => (
                failure::t("maps-search-places"),
                "maps.empty-search".to_string(),
                Role::Button,
                AccessState::new().focusable(true),
            ),
            Hit::ToggleCollapse => (
                if self.panel_collapsed {
                    failure::t("maps-expand-panel")
                } else {
                    failure::t("maps-collapse-panel")
                },
                "maps.panel-collapse".to_string(),
                Role::ToggleButton,
                AccessState::new()
                    .focusable(true)
                    .checked(self.panel_collapsed),
            ),
            Hit::PanelBack => (
                failure::t("maps-back-to-places"),
                "maps.panel.back".to_string(),
                Role::Button,
                AccessState::new().focusable(true),
            ),
            Hit::Mode(mode) => (
                failure::t_args("maps-travel-by", &[("mode", mode.label().into())]),
                format!("maps.mode:{}", mode.id()),
                Role::RadioButton,
                AccessState::new()
                    .focusable(true)
                    .checked(self.model.mode == *mode),
            ),
            Hit::SelectPlace(id) => (
                self.model.place(id).map_or_else(
                    || failure::t("maps-open-place"),
                    |place| {
                        failure::t_args(
                            "maps-open-named-place",
                            &[("name", place.name.clone().into())],
                        )
                    },
                ),
                format!("maps.place:{id}"),
                Role::Button,
                AccessState::new().focusable(true),
            ),
            Hit::SelectPoi(id) => (
                self.pois.iter().find(|poi| poi.id == *id).map_or_else(
                    || failure::t("maps-open-nearby-place"),
                    |poi| {
                        failure::t_args(
                            "maps-open-named-place",
                            &[("name", poi.name.clone().into())],
                        )
                    },
                ),
                format!("maps.poi:{id}"),
                Role::Button,
                AccessState::new().focusable(true),
            ),
            Hit::ClosePoi => (
                failure::t("maps-close-place-details"),
                "maps.poi.close".to_string(),
                Role::Button,
                AccessState::new().focusable(true),
            ),
            Hit::PoiDirections => (
                failure::t("maps-directions-to-place"),
                "maps.poi.directions".to_string(),
                Role::Button,
                AccessState::new().focusable(true),
            ),
            Hit::Recent(query) => (
                failure::t_args("maps-search-again", &[("query", query.clone().into())]),
                format!("maps.recent:{query}"),
                Role::Button,
                AccessState::new().focusable(true),
            ),
            Hit::ClearRecents => (
                failure::t("maps-clear-search-history"),
                "maps.recent.clear".to_string(),
                Role::Button,
                AccessState::new().focusable(true),
            ),
            Hit::ToggleNav => (
                if self.nav_open {
                    failure::t("maps-hide-places")
                } else {
                    failure::t("maps-show-places")
                },
                "maps.navigation".to_string(),
                Role::ToggleButton,
                AccessState::new().focusable(true).checked(self.nav_open),
            ),
            Hit::Directions => (
                failure::t("maps-directions"),
                "maps.directions".to_string(),
                Role::Button,
                AccessState::new().focusable(true),
            ),
            Hit::Transit => (
                failure::t("maps-transit"),
                "maps.transit".to_string(),
                Role::Button,
                AccessState::new().focusable(true),
            ),
            Hit::ToggleSave => (
                failure::t("maps-save-or-unsave"),
                "maps.save".to_string(),
                Role::ToggleButton,
                AccessState::new()
                    .focusable(true)
                    .checked(self.model.selected_place().is_some_and(|place| place.saved)),
            ),
            Hit::ClearRoute => (
                failure::t("maps-clear-route-button"),
                "maps.route.clear".to_string(),
                Role::Button,
                AccessState::new().focusable(true),
            ),
            Hit::NextRoute => (
                failure::t("maps-route-another"),
                "maps.route.another".to_string(),
                Role::Button,
                AccessState::new().focusable(true),
            ),
            Hit::UseMyLocation => (
                failure::t("maps-use-my-location"),
                "maps.location".to_string(),
                Role::Button,
                AccessState::new().focusable(true),
            ),
            Hit::TransitTimeMode(TransitTimeMode::Depart) => (
                failure::t("maps-leave-at"),
                "maps.transit-time:depart".to_string(),
                Role::RadioButton,
                AccessState::new()
                    .focusable(true)
                    .checked(self.model.transit_time.mode == TransitTimeMode::Depart),
            ),
            Hit::TransitTimeMode(TransitTimeMode::Arrive) => (
                failure::t("maps-arrive-by"),
                "maps.transit-time:arrive".to_string(),
                Role::RadioButton,
                AccessState::new()
                    .focusable(true)
                    .checked(self.model.transit_time.mode == TransitTimeMode::Arrive),
            ),
            Hit::TransitTimeDelta(minutes) => (
                failure::t(if *minutes < 0 {
                    "maps-earlier"
                } else {
                    "maps-later"
                }),
                format!("maps.transit-time:delta:{minutes}"),
                Role::Button,
                AccessState::new().focusable(true),
            ),
            Hit::TransitTimeNow => (
                failure::t("maps-reset-to-now"),
                "maps.transit-time:now".to_string(),
                Role::Button,
                AccessState::new().focusable(true),
            ),
            Hit::Journey(index) => (
                failure::t_args("maps-journey-option", &[("number", (index + 1).into())]),
                format!("maps.journey:{index}"),
                Role::RadioButton,
                AccessState::new()
                    .focusable(true)
                    .checked(self.model.transit_choice == *index),
            ),
            Hit::ToggleOverflow => (
                failure::t("maps-more-map-options"),
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
                label: failure::t("maps-history"),
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
                label: failure::t("maps-zoom-in"),
                target: "maps.control:zoom-in".to_string(),
                role: Role::Button,
                state: AccessState::new().focusable(true),
            },
            AccessibilityTarget {
                rect: self.map_controls.rect(MapControlAction::ZoomOut),
                label: failure::t("maps-zoom-out"),
                target: "maps.control:zoom-out".to_string(),
                role: Role::Button,
                state: AccessState::new().focusable(true),
            },
            AccessibilityTarget {
                rect: self.map_controls.rect(MapControlAction::Locate),
                label: failure::t("maps-use-my-location"),
                target: "maps.control:locate".to_string(),
                role: Role::Button,
                state: AccessState::new().focusable(true),
            },
            AccessibilityTarget {
                rect: self.map_controls.rect(MapControlAction::Layers),
                label: failure::t("maps-map-layers"),
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
                label: failure::t("maps-end-navigation"),
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
