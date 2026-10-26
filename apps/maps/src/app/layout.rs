//! Where the map and its floating chrome stand: the margin, the controls column, the panel, the overflow menu, the rects a press is hit-tested against, and the screen or desktop bounds the map may not spill past.

use super::*;

impl MapsApp {
    /// The window margin every floating element keeps — the theme's `s4`.
    pub(super) fn margin(&self) -> f64 {
        f64::from(self.spacing.s4)
    }

    /// The gap between neighbouring floating elements — the theme's `s3`,
    /// the sibling step (HIG §7b), so the panel, the controls and the cards
    /// share one rhythm.
    pub(super) fn gap(&self) -> f64 {
        f64::from(self.spacing.s3)
    }

    /// The control column's geometry (the zoom, locate and traffic buttons).
    pub(super) fn controls(&self) -> ColumnMetrics {
        self.map_controls.metrics()
    }

    pub(super) fn navigation_rect(&self) -> Rect {
        self.navigation_placement
            .rect(self.rect, self.desktop_floating_bounds())
    }

    /// The route card is useful over the map, but duplicates the route summary
    /// and next steps inside the narrow navigation drawer. Hiding it while that
    /// drawer is open avoids covering the fields the user is actively editing.
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

    /// Compact live-navigation surface for the shell floating host. The source
    /// stays app-owned so route changes immediately update an already-detached
    /// overlay instead of snapshotting stale directions.
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
                title: "Active navigation".into(),
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

    /// Whether the layout is narrow enough that the panel becomes an overlay
    /// drawer. Derived from the *current* allocated width every call, so a
    /// resize between breakpoints reflows immediately — never cached.
    pub(super) fn overlay_mode(&self) -> bool {
        // The theme's compact breakpoint; scaling breakpoints with the text
        // is the shell family's policy (NavShell / SidebarDrawer), not an
        // app's — this hand-rolled drawer reads the token and leaves it.
        self.rect.width() < f64::from(self.metrics.compact_breakpoint)
    }

    /// Is the side panel currently on screen? Always in docked layouts; only
    /// when the drawer is open in the overlay layout.
    pub(super) fn panel_visible(&self) -> bool {
        !self.overlay_mode() || self.nav_open
    }

    /// Whether the docked panel column is occupying space with content (docked
    /// and expanded). Bottom map cards keep clear of it; when collapsed the map
    /// reclaims the column so they may use the full width.
    pub(super) fn panel_docked_expanded(&self) -> bool {
        !self.overlay_mode() && !self.panel_collapsed
    }

    /// Panel width for the current window: the list-panel width when docked,
    /// a drawer that stops short of the control column when narrow.
    pub(super) fn panel_w(&self) -> f64 {
        // The panel carries text, so its width grows with the text scale
        // (never shrinks): a 2× row keeps the character budget a 1× row has
        // instead of eliding inside a 1× column. Bounds are combined with
        // `max`/`min` rather than `clamp`: a degenerate rect can put them out
        // of order, and `clamp` panics where these just pick the finite one.
        let grow = f64::from(self.text_scale.clamp(1.0, 4.0));
        let list = f64::from(self.metrics.panel_width_list);
        let avail = (self.rect.width() - 2.0 * self.margin()).max(1.0);
        if self.overlay_mode() {
            // The drawer never slides under the control column.
            let beside_controls = (avail - self.controls().size - self.gap()).max(1.0);
            (list * grow).min(beside_controls)
        } else {
            // Docked: never more than 60 % so the map keeps room beside it.
            (list * grow).min(avail).min(self.rect.width() * 0.6)
        }
    }

    /// The narrow window's top row: the drawer toggle — narrow layouts only.
    pub(super) fn menu_rect(&self) -> Option<Rect> {
        if !self.overlay_mode() {
            return None;
        }
        let side = self.controls().size;
        let x0 = self.rect.x0 + self.margin();
        let y0 = self.rect.y0 + self.margin();
        Some(Rect::new(x0, y0, x0 + side, y0 + side))
    }

    /// The narrow window's search button, beside the drawer toggle.
    pub(super) fn search_button_rect(&self) -> Option<Rect> {
        let menu = self.menu_rect()?;
        let x0 = menu.x1 + f64::from(self.spacing.s2);
        Some(Rect::new(x0, menu.y0, x0 + menu.width(), menu.y1))
    }

    /// The places panel. The narrow drawer ends above the footer row, so the
    /// map's credit and scale stay in view while it is open.
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

    /// The overflow button (attribution, offline maps, licences) in the
    /// bottom-trailing corner.
    pub(super) fn overflow_rect(&self) -> Rect {
        let side = self.controls().size;
        let x1 = self.rect.x1 - self.margin();
        let y1 = self.rect.y1 - self.margin();
        Rect::new(x1 - side, y1 - side, x1, y1)
    }

    /// The leading edge of the map the floating cards keep to: the window
    /// margin, from the docked panel as from the window's edge.
    pub(super) fn map_left_edge(&self) -> f64 {
        if self.panel_docked_expanded() {
            self.panel_rect().x1 + self.margin()
        } else {
            self.rect.x0 + self.margin()
        }
    }

    /// The part of the map nothing floats over: beside the docked panel, clear
    /// of the control column and the footer row, and under the turn card when
    /// it spans the top. Fitting a route frames it here, and the place card
    /// and the loading text centre on it.
    pub(super) fn visible_map_area(&self) -> Rect {
        let gap = self.gap();
        let x0 = self.map_left_edge();
        let mut x1 = self.rect.x1 - self.margin() - self.controls().size - gap;
        let mut y0 = self
            .menu_rect()
            .map_or_else(|| self.rect.y0 + self.margin(), |menu| menu.y1 + gap);
        if self.navigation_surface_visible_in_app() {
            let nav = self.navigation_rect();
            // A card across the top pushes the area down; one in the corner
            // beside a wide map narrows it instead.
            if nav.x0 - gap - x0 >= (x1 - x0) * 0.5 {
                x1 = x1.min(nav.x0 - gap);
            } else {
                y0 = y0.max(nav.y1 + gap);
            }
        }
        let y1 = self.overflow_rect().y0 - gap;
        Rect::new(x0, y0, x1.max(x0 + 1.0), y1.max(y0 + 1.0))
    }

    /// The geographic bounds currently visible in the map viewport.
    pub(super) fn viewport_bounds(&self) -> GeoBounds {
        let proj = self.projection();
        let nw = proj.unproject(Point::new(self.rect.x0, self.rect.y0));
        let se = proj.unproject(Point::new(self.rect.x1, self.rect.y1));
        GeoBounds::new(nw, se)
    }

    /// Expand the viewport bbox by a margin so a small pan reuses the cache.
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

    /// The area the floating map controls (zoom, locate, traffic) lay
    /// themselves out in. Normally the whole window; while the navigation
    /// card floats over the top-right corner, the cluster starts beneath it
    /// so the controls stay visible and reachable rather than being covered.
    /// Paint, pointer hit-testing and the accessibility rects all read the
    /// controls' geometry from this one viewport.
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
        // Beneath the card when the whole column still clears the ⋯ overflow
        // button at the bottom; otherwise beside the card's leading edge when
        // that clears the docked side panel; otherwise the corner stays as it
        // was (the card covers it) rather than laying the cluster out off the
        // bottom of a short window or under the overflow button.
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
