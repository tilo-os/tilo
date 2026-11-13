//! Where the map and its floating chrome sit: the margin, the controls column, the panel, the
//! overflow menu, and the rects a press is hit-tested against.

use super::{
    ColumnMetrics, GeoBounds, Hit, LatLon, MapsApp, NAVIGATION_FLOAT_ID, Point, Projection, Rect,
    SegmentedTouchGroup, SegmentedTouchResult, TransitTimeMode, TravelMode, failure,
};

impl MapsApp {
    /// The window margin every floating element keeps: the theme's `s4`.
    pub(super) fn margin(&self) -> f64 {
        f64::from(self.spacing.s4)
    }

    pub(super) fn gap(&self) -> f64 {
        f64::from(self.spacing.s3)
    }

    pub(super) fn controls(&self) -> ColumnMetrics {
        self.map_controls.metrics()
    }

    pub(super) fn navigation_rect(&self) -> Rect {
        self.navigation_placement
            .rect(self.rect, self.desktop_floating_bounds())
    }

    pub(super) fn navigation_surface_visible_in_app(&self) -> bool {
        self.model.route.is_some()
            && !self.navigation_placement.is_return_pending()
            && matches!(
                self.navigation_placement.attachment(),
                ui::floating_surface::Attachment::App { .. }
            )
            && !(self.overlay_mode() && self.nav_open)
    }

    pub(super) fn desktop_floating_bounds(&self) -> Rect {
        if self.floating_surface_bounds.area() > 0.0 {
            self.floating_surface_bounds
        } else {
            self.rect
        }
    }

    /// The shell's detached copy of the route; the source stays app-owned so a route change updates
    /// it at once.
    pub fn active_navigation_request(
        &self,
    ) -> Option<ui::floating_surface::FloatingSurfaceRequest> {
        self.active_navigation_request_from(self.navigation_rect())
    }

    pub(super) fn active_navigation_request_from(
        &self,
        source_region: Rect,
    ) -> Option<ui::floating_surface::FloatingSurfaceRequest> {
        self.model
            .route
            .as_ref()
            .map(|_route| ui::floating_surface::FloatingSurfaceRequest {
                id: NAVIGATION_FLOAT_ID.into(),
                title: failure::t("maps-active-navigation"),
                role: ui::floating_surface::FloatingRole::Navigation,
                source: ui::floating_surface::FloatingSource::AppElement {
                    app_id: "so.tilo.maps".into(),
                    element_id: "active-navigation".into(),
                    region: source_region,
                },
                preferred_size: self.nav_card_size,
                aspect_ratio: None,
                initial_rect: Some(self.navigation_rect()),
                presentation_rect: Some(self.navigation_rect().inflate(8.0, 8.0)),
                tracking_source: None,
            })
    }

    pub(super) fn view_center(&self) -> Point {
        Point::new(
            f64::midpoint(self.rect.x0, self.rect.x1),
            f64::midpoint(self.rect.y0, self.rect.y1),
        )
    }

    pub(super) fn projection(&self) -> Projection {
        Projection::new(self.model.center, self.model.zoom, self.view_center())
    }

    /// Narrow enough that the panel becomes a drawer; read from the current width on every call so
    /// a resize reflows at once.
    pub(super) fn overlay_mode(&self) -> bool {
        self.rect.width() < f64::from(self.metrics.compact_breakpoint)
    }

    pub(super) fn panel_visible(&self) -> bool {
        !self.overlay_mode() || self.nav_open
    }

    pub(super) fn panel_docked_expanded(&self) -> bool {
        !self.overlay_mode() && !self.panel_collapsed
    }

    pub(super) fn panel_w(&self) -> f64 {
        // The panel carries text, so its width grows with the text scale and never shrinks.
        // `max`/`min` rather than `clamp`, which panics when a degenerate rect puts the bounds out
        // of order.
        let grow = f64::from(self.text_scale.clamp(1.0, 4.0));
        let list = f64::from(self.metrics.panel_width_list);
        let avail = (self.rect.width() - 2.0 * self.margin()).max(1.0);
        if self.overlay_mode() {
            let beside_controls = (avail - self.controls().size - self.gap()).max(1.0);
            (list * grow).min(beside_controls)
        } else {
            (list * grow).min(avail).min(self.rect.width() * 0.6)
        }
    }

    pub(super) fn menu_rect(&self) -> Option<Rect> {
        if !self.overlay_mode() {
            return None;
        }
        let side = self.controls().size;
        let x0 = self.rect.x0 + self.margin();
        let y0 = self.rect.y0 + self.margin();
        Some(Rect::new(x0, y0, x0 + side, y0 + side))
    }

    pub(super) fn search_button_rect(&self) -> Option<Rect> {
        let menu = self.menu_rect()?;
        let x0 = menu.x1 + f64::from(self.spacing.s2);
        Some(Rect::new(x0, menu.y0, x0 + menu.width(), menu.y1))
    }

    pub(super) fn panel_rect(&self) -> Rect {
        let x0 = self.rect.x0 + self.margin();
        let top = self
            .menu_rect()
            .map_or_else(|| self.rect.y0 + self.margin(), |menu| menu.y1 + self.gap());
        let bottom = if self.overlay_mode() {
            self.overflow_rect().y0 - self.gap()
        } else {
            self.rect.y1 - self.margin()
        };
        Rect::new(x0, top, x0 + self.panel_w(), bottom.max(top))
    }

    pub(super) fn overflow_rect(&self) -> Rect {
        let side = self.controls().size;
        let x1 = self.rect.x1 - self.margin();
        let y1 = self.rect.y1 - self.margin();
        Rect::new(x1 - side, y1 - side, x1, y1)
    }

    pub(super) fn map_left_edge(&self) -> f64 {
        if self.panel_docked_expanded() {
            self.panel_rect().x1 + self.margin()
        } else {
            self.rect.x0 + self.margin()
        }
    }

    pub(super) fn visible_map_area(&self) -> Rect {
        let gap = self.gap();
        let x0 = self.map_left_edge();
        let mut x1 = self.rect.x1 - self.margin() - self.controls().size - gap;
        let mut y0 = self
            .menu_rect()
            .map_or_else(|| self.rect.y0 + self.margin(), |menu| menu.y1 + gap);
        if self.navigation_surface_visible_in_app() {
            let nav = self.navigation_rect();
            if nav.x0 - gap - x0 >= (x1 - x0) * 0.5 {
                x1 = x1.min(nav.x0 - gap);
            } else {
                y0 = y0.max(nav.y1 + gap);
            }
        }
        let y1 = self.overflow_rect().y0 - gap;
        Rect::new(x0, y0, x1.max(x0 + 1.0), y1.max(y0 + 1.0))
    }

    pub(super) fn viewport_bounds(&self) -> GeoBounds {
        let proj = self.projection();
        let nw = proj.unproject(Point::new(self.rect.x0, self.rect.y0));
        let se = proj.unproject(Point::new(self.rect.x1, self.rect.y1));
        GeoBounds::new(nw, se)
    }

    /// The viewport grown by a margin, so a small pan reuses the cache.
    pub(super) fn padded_viewport_bounds(&self) -> GeoBounds {
        let v = self.viewport_bounds();
        let dlat = (v.max.lat - v.min.lat) * 0.25;
        let dlon = (v.max.lon - v.min.lon) * 0.25;
        GeoBounds::new(
            LatLon::new(v.min.lat - dlat, v.min.lon - dlon),
            LatLon::new(v.max.lat + dlat, v.max.lon + dlon),
        )
    }

    pub(super) fn segmented_rects(&self) -> (Vec<Rect>, Vec<Rect>) {
        if self.info_sheet.modal_capture().is_some()
            || self.offline_panel.is_open()
            || self.overflow.is_open()
        {
            return (Vec::new(), Vec::new());
        }
        let modes = self
            .click_targets
            .iter()
            .filter_map(|(rect, hit)| matches!(hit, Hit::Mode(_)).then_some(*rect))
            .collect();
        let times = self
            .click_targets
            .iter()
            .filter_map(|(rect, hit)| matches!(hit, Hit::TransitTimeMode(_)).then_some(*rect))
            .collect();
        (modes, times)
    }

    pub(super) fn handle_segmented_touch(
        &mut self,
        touch: &winit::event::Touch,
        scale: f64,
    ) -> bool {
        let (modes, times) = self.segmented_rects();
        let selected = TravelMode::ALL
            .iter()
            .position(|mode| *mode == self.model.mode)
            .unwrap_or(0);
        let groups = [
            SegmentedTouchGroup::new(0, &modes, selected),
            SegmentedTouchGroup::new(
                1,
                &times,
                usize::from(self.model.transit_time.mode == TransitTimeMode::Arrive),
            ),
        ];
        let result = self.segmented_touch.handle(touch, scale, &groups);
        if let SegmentedTouchResult::Selected { group, index } = result {
            let hit = if group == 0 {
                TravelMode::ALL.get(index).copied().map(Hit::Mode)
            } else {
                [TransitTimeMode::Depart, TransitTimeMode::Arrive]
                    .get(index)
                    .copied()
                    .map(Hit::TransitTimeMode)
            };
            if let Some(hit) = hit {
                self.dispatch(&hit);
            }
        }
        result.handled()
    }

    pub(super) fn map_controls_viewport(&self) -> Rect {
        let viewport = self.rect;
        if !self.navigation_surface_visible_in_app() {
            return viewport;
        }
        let margin = self.margin();
        let gap = self.gap();
        let controls = self.controls();
        let nav = self.navigation_rect();
        let column_h = margin + self.map_controls.column_height();
        let column = Rect::new(
            self.rect.x1 - margin - controls.size,
            self.rect.y0,
            self.rect.x1,
            self.rect.y0 + column_h,
        );
        if nav.intersect(column).area() <= 0.0 {
            return viewport;
        }
        let floor = self.overflow_rect().y0 - gap;
        if nav.y1 < self.rect.y1 && nav.y1 + column_h <= floor {
            return Rect::new(viewport.x0, nav.y1, viewport.x1, viewport.y1);
        }
        let panel_right = if self.panel_docked_expanded() {
            self.panel_rect().x1 + gap
        } else {
            self.rect.x0 + margin
        };
        if nav.x0 - gap - controls.size >= panel_right {
            return Rect::new(viewport.x0, viewport.y0, nav.x0 - gap + margin, viewport.y1);
        }
        viewport
    }
}
