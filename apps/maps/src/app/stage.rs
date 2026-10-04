//! Staging a screenshot scenario's view state: an open panel, a menu, a
//! search in progress, a detached navigation card. The scenario's places and
//! plans are already the resource's, written by its actions
//! ([`crate::scenario`]); what is staged here is this window's alone.

use vello::kurbo::{Point, Rect};

#[cfg(test)]
use super::TravelMode;
use super::{Endpoint, FloatingSurfaceProvider, LatLon, MapInfo, MapsApp, Panel, Widget, sample};

impl MapsApp {
    /// Stage `scenario`'s view state, once its actions are laid out.
    pub(crate) fn stage_scenario(&mut self, scenario: &str) {
        match scenario {
            "explore" => self.set_active_panel(Panel::Explore),
            "directions" | "transit" | "transit-arrive" => self.set_active_panel(Panel::Directions),
            "edit-from" => self.stage_edit_origin(),
            "search" => self.stage_search(),
            "pois" => self.stage_pois(),
            "traffic" | "traffic-empty" => self.stage_traffic(),
            "offline" => self.stage_offline(),
            "menu" => self.stage_overflow(),
            "attribution" => self.stage_attribution(),
            "error" => self.stage_error(),
            "nav-closed" => self.set_nav_open(false),
            "floating-navigation" => self.stage_navigation("detached"),
            _ => {
                if let Some(stage) = scenario.strip_prefix("floating-navigation-") {
                    self.stage_navigation(stage);
                }
            }
        }
    }

    /// Scenario stage: force the overlay drawer open/closed so both the
    /// map-first and panel-open states can be captured headlessly.
    pub(super) fn set_nav_open(&mut self, open: bool) {
        self.nav_open = open;
    }

    /// Scenario stage for the shared attribution dialog.
    pub(super) fn stage_attribution(&mut self) {
        self.info_sheet.open(MapInfo::Attribution);
    }

    /// Scenario stage for an honest recoverable network failure. This
    /// keeps visual regression review from treating loading and error states as
    /// interchangeable, while the production app still obtains this text only
    /// from a real worker failure.
    pub(super) fn stage_error(&mut self) {
        self.model.status = "Map imagery unavailable offline — reconnect and try again".into();
    }

    /// Scenario stage: force the active side panel so each tab can be
    /// captured headlessly. Offline sample data already populates every panel,
    /// so this never triggers a fetch.
    pub(super) fn set_active_panel(&mut self, panel: Panel) {
        self.model.panel = panel;
    }

    /// Scenario stage: force the Directions "From" field into its editing
    /// state with a populated geocoded dropdown, so the typeable-origin flow can
    /// be captured headlessly. Uses offline local matches only — no network.
    pub(super) fn stage_edit_origin(&mut self) {
        self.model.panel = Panel::Directions;
        self.model.origin_query = "hauptbahnhof".to_string();
        self.endpoints.begin_edit(Endpoint::Origin);
        self.endpoints
            .sync_queries(&self.model.origin_query, &self.model.dest_query);
        self.endpoints
            .set_results(Endpoint::Origin, self.local_matches("hauptbahnhof"));
    }

    /// Scenario stage: seed the live POI marker layer from bundled sample
    /// data and open the info card for one of them, so the themed POI markers and
    /// the info card can be captured headlessly. No network.
    pub(super) fn stage_pois(&mut self) {
        self.pois = sample::pois();
        self.selected_poi = Some("osm-node-203".to_string()); // Hofbräuhaus
        // Frame the map so the POI cluster sits in the upper map area and the
        // bottom-anchored info card is clear of it. Zoom past the icon + label
        // threshold so every marker carries its category pictogram and name.
        self.model.center = LatLon::new(48.1381, 11.5770);
        self.model.zoom = 16.7;
        self.model.selected = None;
        // Focus the Explore panel on the live NEARBY list for the shot: drop the
        // stale committed search/results so the nearby places lead the panel and
        // the selected POI's row is the highlighted one.
        self.model.results.clear();
        self.model.query.clear();
        self.model.panel = Panel::Explore;
    }

    /// Scenario stage: open the offline-maps overlay with a couple of
    /// already-saved regions and a plausible on-disk size, so the download UI
    /// AND the managed-region list render headlessly. No network, no disk I/O —
    /// the regions are bundled sample metadata, not real cache entries.
    pub(super) fn stage_offline(&mut self) {
        self.model.panel = Panel::Explore;
        self.model.selected = None;
        self.model.regions = sample::regions();
        // Demonstrate the download-by-place search: a typed city with candidate
        // results in the shared dropdown (offline sample data — no network).
        self.offline_panel
            .set_demo(47_542_000, "Mün", sample::download_regions());
    }

    /// Scenario stage: open the ⋯ overflow menu so its Attribution /
    /// Offline maps / Licenses options render expanded over the map, anchored
    /// above its bottom-right button.
    pub(super) fn stage_overflow(&mut self) {
        let b = self.overflow_rect();
        self.overflow.open_at([b.x1 as f32, b.y0 as f32]);
        let mr = self.overflow.get_rect();
        self.overflow.open_at([
            (b.x1 - mr.width()) as f32,
            (b.y0 - mr.height() - 6.0) as f32,
        ]);
    }

    /// Scenario stage for the production navigation renderer and its real
    /// shell → app ownership edge. Staged after layout so the `Suspended` host
    /// path uses authoritative app-local coordinates.
    pub(super) fn stage_navigation(&mut self, stage: &str) {
        if stage == "attached" {
            self.navigation_placement
                .pin_handoff_for_visual_review(1_000);
            return;
        }
        let detached = Rect::new(820.0, 120.0, 1_180.0, 276.0);
        self.navigation_placement.detach_to_desktop(detached);
        self.navigation_placement.set_desktop_transfer_covered(true);
        self.navigation_placement.confirm_desktop_adoption();

        if stage == "hover" {
            self.navigation_end_hovered = true;
        }

        if matches!(stage, "returning" | "returned") {
            let pointer = Point::new(detached.center().x, detached.y0 + 16.0);
            self.navigation_placement.begin_drag(
                pointer,
                self.rect,
                self.desktop_floating_bounds(),
            );
            let _ = self.navigation_placement.update_drag(
                Point::new(720.0, 360.0),
                self.rect,
                self.desktop_floating_bounds(),
            );
        }

        if stage == "returned"
            && let Some(id) = self.active_navigation_request().map(|request| request.id)
        {
            <Self as FloatingSurfaceProvider>::on_floating_surface_host_message(
                self,
                &ui::floating_surface::FloatingSurfaceHostMessage::Suspended { id },
            );
        }

        let handoff_phase = if matches!(stage, "returning" | "returned") {
            500
        } else {
            1_000
        };
        self.navigation_placement
            .pin_handoff_for_visual_review(handoff_phase);
    }

    /// Scenario stage: open the floating search-results overlay populated
    /// with proximity-ranked matches, so the search-as-you-type dropdown can be
    /// captured headlessly. Offline local data only — no network.
    pub(super) fn stage_search(&mut self) {
        self.model.panel = Panel::Explore;
        self.search.set_focused(true);
        self.model.query = "Marienplatz".to_string();
        let mut results = self.model.places.clone();
        self.rank_by_proximity(&mut results);
        self.search.replace_results(results);
    }

    /// Select a travel mode in the Directions panel of this window only, as
    /// tests drive it. Offline sample data already populates the
    /// route/departures, so nothing fetches.
    #[cfg(test)]
    pub(super) fn set_travel_mode(&mut self, mode: TravelMode) {
        self.model.mode = mode;
        self.model.panel = Panel::Directions;
        // A transit journey draws no road polyline; drop the sample road route
        // so the map under the departures stays clean.
        if mode.is_transit() {
            self.model.route = None;
        }
    }

    /// Scenario stage: turn the traffic overlay on so the coloured
    /// congestion segments (or, in an empty map, the honest "No traffic data"
    /// state) can be captured headlessly.
    pub(super) fn stage_traffic(&mut self) {
        self.set_traffic(true);
    }
}
