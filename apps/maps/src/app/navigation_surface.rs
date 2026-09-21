use super::{
    Affine, Fill, MapsApp, NAVIGATION_FLOAT_ID, Point, Rect, Scene, Stroke, TextRenderer,
    TextWeight, Theme, draw, geo,
};

impl MapsApp {
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
                preferred_size: vello::kurbo::Size::new(360.0, 156.0),
                aspect_ratio: None,
                initial_rect: Some(self.navigation_rect()),
                presentation_rect: Some(self.navigation_rect().inflate(8.0, 8.0)),
                tracking_source: None,
            })
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

    #[allow(
        clippy::too_many_lines,
        reason = "the navigation card is one cohesive painted surface with shared geometry for its drag and end-navigation targets"
    )]
    pub(super) fn draw_navigation_surface(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let Some(route) = self.model.route.clone() else {
            self.navigation_drag_handle = Rect::ZERO;
            self.navigation_end_rect = Rect::ZERO;
            self.navigation_end_hovered = false;
            return;
        };
        let rect = self.navigation_rect();
        let visual =
            self.navigation_placement
                .presentation_transform(transform, rect, theme.reduce_motion);
        let c = &theme.tokens.colors;
        let shadow =
            ui::surface::soft_outline(rect.inflate(5.0, 5.0) + vello::kurbo::Vec2::new(0.0, 2.0));
        scene.fill(
            Fill::NonZero,
            visual,
            draw::alpha(c.neutral[0], 0.30),
            None,
            &shadow,
        );
        let body = ui::surface::soft_outline(rect);
        scene.fill(Fill::NonZero, visual, c.surfaces.raised, None, &body);
        scene.stroke(&Stroke::new(1.0), visual, c.border_standard, None, &body);
        let end = Rect::new(rect.x1 - 58.0, rect.y0 + 6.0, rect.x1 - 8.0, rect.y0 + 30.0);
        self.navigation_end_rect = end;
        let handle = Rect::new(rect.x0, rect.y0, end.x0 - 4.0, rect.y0 + 34.0);
        self.navigation_drag_handle = handle;
        draw::text_w(
            scene,
            tr,
            visual,
            "Navigation",
            rect.x0 + 16.0,
            rect.y0 + 10.0,
            12.0,
            theme.text_dim,
            TextWeight::Medium,
        );
        ui::surface::floating_drag_grip(scene, visual, rect, theme.text_dim);

        // Ending a live activity is intentionally explicit. At rest this reads
        // as quiet chrome; hover adds a soft danger wash without changing the
        // button's geometry (and therefore without making the title jump).
        let end_fill = if self.navigation_end_hovered {
            draw::alpha(c.semantic.danger, 0.16)
        } else {
            draw::alpha(c.neutral[3], 0.62)
        };
        let end_stroke = if self.navigation_end_hovered {
            draw::alpha(c.semantic.danger, 0.58)
        } else {
            c.border_standard
        };
        let end_text = if self.navigation_end_hovered {
            c.semantic.danger
        } else {
            theme.text_dim
        };
        draw::fill_rrect(scene, visual, end, 10.0, end_fill);
        draw::stroke_rrect(scene, visual, end, 10.0, 1.0, end_stroke);
        draw::fill_rrect(
            scene,
            visual,
            Rect::new(
                end.x0 + 9.0,
                end.center().y - 2.5,
                end.x0 + 14.0,
                end.center().y + 2.5,
            ),
            1.5,
            end_text,
        );
        draw::text_w(
            scene,
            tr,
            visual,
            "End",
            end.x0 + 19.0,
            end.y0 + 6.0,
            11.0,
            end_text,
            TextWeight::Medium,
        );

        let instruction = route.steps.first();
        let glyph = instruction.map_or("↑", |step| step.maneuver.glyph());
        let text = instruction.map_or("Continue on the route", |step| step.instruction.as_str());
        let badge = Rect::new(
            rect.x0 + 16.0,
            rect.y0 + 48.0,
            rect.x0 + 52.0,
            rect.y0 + 84.0,
        );
        draw::fill_circle(
            scene,
            visual,
            badge.center(),
            18.0,
            draw::alpha(theme.accent_primary, 0.18),
        );
        draw::text_center(
            scene,
            tr,
            visual,
            glyph,
            badge.x0,
            badge.x1,
            badge.y0 + 8.0,
            17.0,
            theme.accent_primary,
        );
        draw::text_clip(
            scene,
            tr,
            visual,
            text,
            rect.x0 + 64.0,
            rect.y0 + 47.0,
            15.0,
            theme.text_primary,
            rect.width() - 82.0,
        );
        let next_distance = instruction.map_or(route.distance_m, |step| step.distance_m);
        draw::text(
            scene,
            tr,
            visual,
            &geo::fmt_distance(next_distance),
            rect.x0 + 64.0,
            rect.y0 + 70.0,
            12.0,
            theme.text_dim,
        );
        draw::line(
            scene,
            visual,
            Point::new(rect.x0 + 16.0, rect.y0 + 98.0),
            Point::new(rect.x1 - 16.0, rect.y0 + 98.0),
            1.0,
            c.border_standard,
        );
        draw::text_w(
            scene,
            tr,
            visual,
            &geo::fmt_duration(route.duration_s),
            rect.x0 + 16.0,
            rect.y0 + 114.0,
            16.0,
            theme.accent_primary,
            TextWeight::Bold,
        );
        draw::text_right(
            scene,
            tr,
            visual,
            &format!("{} remaining", geo::fmt_distance(route.distance_m)),
            rect.x1 - 16.0,
            rect.y0 + 116.0,
            12.0,
            theme.text_dim,
        );
    }
}
