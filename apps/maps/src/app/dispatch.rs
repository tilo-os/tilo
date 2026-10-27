//! The one door an action comes through: a menu row, a control, an offline verb or a touch, turned into the state or resource change it names.

use super::*;

impl MapsApp {
    pub(super) fn dispatch(&mut self, hit: &Hit) {
        match hit {
            Hit::Consume => {}
            Hit::OpenSearch | Hit::OpenEmptySearch => {
                self.search_opener
                    .open(ui::search::SearchRequest::this_app());
            }
            Hit::ToggleCollapse => {
                self.panel_collapsed = !self.panel_collapsed;
            }
            Hit::PanelBack => {
                // Return from the Directions view to the plain places list. The
                // route stays in the model (it keeps drawing on the map); only
                // the panel view changes — this is navigation, not a tab.
                self.model.panel = Panel::Explore;
                self.endpoints.dismiss();
                self.scroll = 0.0;
            }
            Hit::Mode(m) => self.set_mode(*m),
            Hit::SelectPlace(id) => {
                self.select_place(id);
            }
            Hit::SelectPoi(id) => self.select_poi_from_list(id),
            Hit::ClosePoi => {
                self.selected_poi = None;
            }
            Hit::PoiDirections => self.poi_directions(),
            Hit::Recent(q) => {
                self.search_opener
                    .open(ui::search::SearchRequest::this_app().with_query(q.clone()));
            }
            Hit::ClearRecents => {
                self.model.recents.clear();
                self.perform(Workspace::CLEAR_RECENTS.invoke());
            }
            Hit::ToggleNav => {
                self.nav_open = !self.nav_open;
            }
            Hit::CloseNav => {
                self.nav_open = false;
            }
            Hit::Directions => self.enter_directions(),
            Hit::Transit => self.enter_transit(),
            Hit::ToggleSave => self.toggle_save_selected(),
            Hit::ClearRoute => self.clear_route(),
            Hit::NextRoute => self.next_route(),
            Hit::UseMyLocation => self.use_my_location(),
            Hit::TransitTimeMode(m) => {
                let mut value = self.model.transit_time;
                value.mode = *m;
                self.set_transit_time(value);
                self.replan_for_time();
            }
            Hit::TransitTimeDelta(d) => {
                let mut value = self.model.transit_time;
                value.offset_min = (value.offset_min + *d).clamp(0, 1440);
                self.set_transit_time(value);
                self.replan_for_time();
            }
            Hit::TransitTimeNow => {
                self.set_transit_time(crate::model::TransitTime::default());
                self.replan_for_time();
            }
            Hit::Journey(i) => {
                self.model.transit_choice = *i;
                // Frame the newly-selected itinerary on the map (its legs are
                // what draws now), not just expand the panel card.
                self.fit_current();
            }
            Hit::ToggleOverflow => {
                if self.menu_open(MapMenu::More) {
                    self.overflow.close();
                } else {
                    self.open_overflow();
                }
            }
        }
    }

    pub(super) fn dispatch_map_control(&mut self, action: MapControlAction) {
        match action {
            MapControlAction::History => self.history.activate(),
            MapControlAction::ZoomIn => self.zoom_by(1.0, self.visible_map_area().center()),
            MapControlAction::ZoomOut => self.zoom_by(-1.0, self.visible_map_area().center()),
            MapControlAction::Locate => self.use_my_location(),
            MapControlAction::Layers => {
                if self.menu_open(MapMenu::Layers) {
                    self.overflow.close();
                } else {
                    self.open_layers();
                }
            }
        }
    }

    /// Apply an effect requested by the self-contained offline panel.
    pub(super) fn dispatch_offline(&mut self, action: OfflineAction) {
        match action {
            OfflineAction::Close => self.offline_panel.close(),
            OfflineAction::ZoomDelta(delta) => self.offline_panel.adjust_zoom(delta),
            OfflineAction::Download if !self.tiles.allows_prefetch() => {
                self.model.status = areas_refused();
            }
            OfflineAction::Download => {
                let request = self.offline_panel.download_request(
                    self.viewport_bounds(),
                    TileLayer::tile_zoom(self.model.zoom),
                    self.rect,
                );
                let count = self
                    .tiles
                    .start_download(request.bounds, request.z0, request.z1);
                self.record_region(
                    request.bounds,
                    request.z0,
                    request.z1,
                    count,
                    request.name.clone(),
                );
                self.model.status = request.name.map_or_else(
                    || {
                        failure::t_args(
                            "maps-offline-downloading",
                            &[("count", i64::from(count).into())],
                        )
                    },
                    |name| {
                        failure::t_args(
                            "maps-offline-downloading-named",
                            &[("count", i64::from(count).into()), ("name", name.into())],
                        )
                    },
                );
            }
            OfflineAction::StopDownload => {
                if self.tiles.stop_download() {
                    self.model.status = failure::t("maps-offline-stopped");
                }
            }
            OfflineAction::DeleteRegion(id) => {
                if let Err(error) = self.delete_region(&id) {
                    self.model.status = error;
                }
            }
            OfflineAction::ClearCache => self.clear_offline_areas(),
            OfflineAction::Consume => {}
        }
    }
}
