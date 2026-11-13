//! The map's frame: its layers and markers, the top row and footer, the overflow menu, and the
//! traffic key.

use super::{
    Affine, Button, ButtonFace, ButtonRole, ColumnMetrics, Congestion, Hit, Icon, MapMenu,
    MapPalette, MapView, MapsApp, PaintCtx, Panel, Point, Rect, Scene, TextRenderer, TextRole,
    Theme, TileStyle, Widget, canvas, draw, failure, geo, imagery_hint, on_panel,
};

impl MapsApp {
    pub(super) fn hovered(&self, rect: Rect) -> bool {
        !self.dragging && rect.contains(self.last_mouse)
    }

    pub(super) fn icon_button(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        rect: Rect,
        (icon, name): (Icon, &str),
        role: ButtonRole,
        on: bool,
        hit: Hit,
    ) {
        let face = ButtonFace::icon_only(rect, icon, name)
            .role(role)
            .hovered(self.hovered(rect))
            .toggled(on);
        Button::paint(ctx.scene, ctx.transform, ctx.text, ctx.theme, &face);
        self.click_targets.push((face.hit_rect(ctx.theme), hit));
    }

    pub(super) fn text_button(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        face: ButtonFace<'_>,
        rect: Rect,
        hit: Option<Hit>,
    ) {
        let face = face.hovered(self.hovered(rect) && hit.is_some());
        Button::paint(ctx.scene, ctx.transform, ctx.text, ctx.theme, &face);
        if let Some(hit) = hit {
            self.click_targets.push((rect, hit));
        }
    }

    pub(super) fn draw_app(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        self.click_targets.clear();
        self.text_scale = theme.text_scale;
        self.ui_scale = f64::from(theme.ui_scale);
        self.map_controls.set_metrics(ColumnMetrics::of(theme));
        self.map_controls.set_viewport(self.map_controls_viewport());
        if self.pending_fit && self.rect.area() > 0.0 {
            self.pending_fit = false;
            self.fit_current();
        }
        if self.rect.area() > 0.0
            && let Some(pos) = self.pending_focus.take()
        {
            self.center_on(pos);
        }
        // Point the raster layer at the theme's basemap style before any tile is drawn, so imagery
        // and cartography never mix tones.
        self.tiles.set_style(TileStyle::from_theme(theme));
        let pal = MapPalette::from_theme(theme);
        let overlay = self.overlay_mode();

        let mut chrome = Scene::new();
        self.chrome_rects.clear();
        self.poi_card_rect = Rect::ZERO;
        self.draw_chrome(&mut chrome, t, tr, theme, overlay);
        if std::mem::take(&mut self.pending_poi_focus) {
            self.focus_selected_poi();
        }

        let proj = self.projection();
        let clip = self.rect;
        let view = MapView {
            proj: &proj,
            pal: &pal,
            clip,
            chrome: &[],
        };
        let painted_tiles = self.draw_map_layers(&mut PaintCtx::new(scene, t, tr, theme), view);
        let keep_clear = f64::from(theme.tokens.spacing.s1);
        let floating: Vec<Rect> = self
            .chrome_rects
            .iter()
            .map(|rect| rect.inflate(keep_clear, keep_clear))
            .collect();
        self.draw_markers(
            &mut PaintCtx::new(scene, t, tr, theme),
            MapView {
                chrome: &floating,
                ..view
            },
        );
        if !painted_tiles && self.basemap.is_empty() && !(overlay && self.nav_open) {
            self.draw_imagery_hint(scene, t, tr, theme);
        }
        scene.append(&chrome, None);
    }

    pub(super) fn draw_map_layers(&mut self, ctx: &mut PaintCtx<'_>, view: MapView<'_>) -> bool {
        let MapView {
            proj, pal, clip, ..
        } = view;
        let theme = ctx.theme;
        let view_center = self.view_center();
        let painted_tiles = canvas::draw_tiles(
            ctx.scene,
            ctx.transform,
            clip,
            &mut self.tiles,
            &self.model,
            view_center,
            pal,
        );
        if !painted_tiles {
            if self.basemap.is_empty() {
                canvas::draw_grid_substrate(
                    ctx.scene,
                    ctx.transform,
                    clip,
                    &self.model,
                    view_center,
                    pal,
                );
            } else {
                canvas::draw_basemap(ctx.scene, ctx.transform, clip, proj, &self.basemap, pal);
            }
        }
        if self.map_controls.traffic_enabled() {
            canvas::draw_traffic(ctx.scene, ctx.transform, proj, &self.traffic, pal);
        }
        if let Some(route) = &self.model.route {
            canvas::draw_route(ctx.scene, ctx.transform, proj, route, pal);
        }
        if self.model.mode.is_transit()
            && let Some(j) = self.model.transit.get(self.model.transit_choice)
        {
            canvas::draw_transit(ctx.scene, ctx.transform, proj, j, theme, pal, clip);
        }
        painted_tiles
    }

    pub(super) fn draw_markers(&self, ctx: &mut PaintCtx<'_>, view: MapView<'_>) {
        let visible_pois = self.visible_poi_indices();
        canvas::draw_pois(
            ctx,
            view,
            &self.pois,
            &visible_pois,
            self.selected_poi.as_deref(),
            &self.model,
        );
        canvas::draw_markers(ctx, view, &self.model);
    }

    pub(super) fn floats(&mut self, rect: Rect) {
        if rect.area() > 0.0 {
            self.chrome_rects.push(rect);
        }
    }

    pub(super) fn draw_chrome(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        overlay: bool,
    ) {
        let drawer_open = overlay && self.nav_open;
        self.map_controls.draw(scene, t, tr, theme);
        let controls: Vec<Rect> = self.map_controls.rects().collect();
        for rect in controls {
            self.floats(rect);
        }
        if !drawer_open {
            self.draw_map_footer(scene, t, tr, theme);
            self.draw_overflow(scene, t, tr, theme);
        }
        if drawer_open {
            ui::overlay::scrim::modal(scene, t, self.rect, 1.0, theme);
            self.click_targets.push((self.rect, Hit::CloseNav));
            self.draw_map_footer(scene, t, tr, theme);
            self.draw_overflow(scene, t, tr, theme);
            // Stale hit and accessibility geometry must not outlive the turn card the open drawer
            // hides.
            self.navigation_drag_handle = Rect::ZERO;
            self.navigation_end_rect = Rect::ZERO;
            self.navigation_end_hovered = false;
        } else {
            self.draw_traffic_status(scene, t, tr, theme);
            self.draw_poi_card(scene, t, tr, theme);
            let uncovered_desktop_copy = self.model.route.is_some()
                && matches!(
                    self.navigation_placement.attachment(),
                    ui::floating_surface::Attachment::Desktop { .. }
                )
                && !self.navigation_placement.is_desktop_covered();
            if self.navigation_surface_visible_in_app() || uncovered_desktop_copy {
                self.draw_navigation_surface(scene, t, tr, theme);
            }
        }
        if self.panel_visible() {
            self.draw_panel(scene, t, tr, theme);
            if matches!(self.model.panel, Panel::Directions) {
                self.endpoints.draw_overlay(scene, t, tr, theme);
            }
        }
        self.draw_top_row(scene, t, tr, theme);
        if self.overflow.is_open() {
            self.floats(self.overflow.get_rect());
        }
        self.map_controls.draw_tooltip(scene, t, tr, theme);
    }

    pub(super) fn draw_overflow(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let r = self.overflow_rect();
        let open = self.menu_open(MapMenu::More);
        self.floats(r);
        self.icon_button(
            &mut PaintCtx::new(scene, t, tr, theme),
            r,
            (Icon::MoreHorizontal, &failure::t("maps-more")),
            ButtonRole::Secondary,
            open,
            Hit::ToggleOverflow,
        );
    }

    pub(super) fn draw_top_row(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let Some(menu) = self.menu_rect() else { return };
        let name = failure::t(if self.nav_open {
            "maps-hide-places"
        } else {
            "maps-show-places"
        });
        let open = self.nav_open;
        self.icon_button(
            &mut PaintCtx::new(scene, t, tr, theme),
            menu,
            (Icon::PanelLeft, &name),
            ButtonRole::Secondary,
            open,
            Hit::ToggleNav,
        );
        if let Some(search) = self.search_button_rect() {
            self.icon_button(
                &mut PaintCtx::new(scene, t, tr, theme),
                search,
                (Icon::Search, &failure::t("maps-search-places")),
                ButtonRole::Secondary,
                false,
                Hit::OpenSearch,
            );
            self.floats(menu.union(search));
        }
    }

    pub(super) fn draw_map_footer(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let sp = &theme.tokens.spacing;
        let (s2, s3) = (f64::from(sp.s2), f64::from(sp.s3));
        let ink = on_panel(theme);
        let caption = theme.text(TextRole::Caption);
        let numeric = caption.clone().numeric();
        let overflow = self.overflow_rect();
        let right = overflow.x0 - self.gap();
        let bar_max = f64::from(sp.s16) + f64::from(sp.s8);
        let (label, bar_w) = geo::scale_bar(self.model.center, self.model.zoom, bar_max);
        let label_w = draw::measure_w(tr, &label, &numeric);
        let credit = crate::tiles::TILE_ATTRIBUTION;
        let credit_w = draw::measure_w(tr, credit, &caption);
        let scale_w = s3 + label_w + s2 + bar_w + s3;
        let both_w = s3 + credit_w + s3 + label_w + s2 + bar_w + s3;
        let left = self.map_left_edge();
        let together = right - both_w >= left;
        let scale_x0 = if together {
            right - both_w
        } else {
            right - scale_w
        };
        let pill = Rect::new(scale_x0, overflow.y0, right, overflow.y1);
        self.floats(pill);
        ui::widget::floating_surface::paint(scene, t, pill, theme);
        let mut ctx = PaintCtx::new(scene, t, tr, &ink);
        let top = draw::vmid(ctx.text, pill.y0, pill.height(), &caption);
        let mut x = pill.x0 + s3;
        if together {
            draw::text(&mut ctx, credit, Point::new(x, top), &caption, ink.text_dim);
            x += credit_w + s3;
        }
        draw::text(&mut ctx, &label, Point::new(x, top), &numeric, ink.text_dim);
        x += label_w + s2;
        let cy = pill.center().y;
        let tick = f64::from(sp.s1);
        let stroke = f64::from(theme.tokens.metrics.chrome_rule_px) * 1.5;
        draw::line(
            ctx.scene,
            t,
            Point::new(x, cy),
            Point::new(x + bar_w, cy),
            stroke,
            ink.text_primary,
        );
        for end in [x, x + bar_w] {
            draw::line(
                ctx.scene,
                t,
                Point::new(end, cy - tick),
                Point::new(end, cy + tick),
                stroke,
                ink.text_primary,
            );
        }
        if !together {
            let h = pill.height();
            let credit_pill = Rect::new(
                (right - (credit_w + 2.0 * s3)).max(left),
                pill.y0 - self.gap() - h,
                right,
                pill.y0 - self.gap(),
            );
            self.floats(credit_pill);
            ui::widget::floating_surface::paint(ctx.scene, t, credit_pill, theme);
            let top = draw::vmid(ctx.text, credit_pill.y0, h, &caption);
            draw::text_clip(
                &mut ctx,
                credit,
                Point::new(credit_pill.x0 + s3, top),
                &caption,
                ink.text_dim,
                credit_pill.width() - 2.0 * s3,
            );
        }
    }

    pub(super) fn publish_status(&mut self) {
        if self.model.status != self.status_said {
            self.status_said.clone_from(&self.model.status);
            if self.status_said.is_empty() {
                self.status_notice.dismiss();
            } else if self.status_said.ends_with('…') {
                self.status_notice.show_progress(self.status_said.clone());
            } else {
                self.status_notice.show(self.status_said.clone());
            }
        }
        self.status_notice
            .keep_while(self.status_said.ends_with('…'));
        self.status_notice.expire();
        self.status_notice.publish(|text| {
            ui::window_toolbar::ToolbarNotice::new(text.clone(), ui::widget::toast::ToastKind::Info)
        });
    }

    /// Say the imagery is loading, so a first-run or offline map is never a silent void.
    pub(super) fn draw_imagery_hint(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let fault = self.tiles.failure().filter(|_| !self.tiles.has_inflight());
        let msg = imagery_hint(self.tiles.can_fetch(), fault);
        let area = self.visible_map_area();
        let style = theme.text(TextRole::Body);
        let mut ctx = PaintCtx::new(scene, t, tr, theme);
        let h = draw::line_h(ctx.text, &style);
        let low = area.y0 + area.height() * 0.75;
        draw::text_center(
            &mut ctx,
            &msg,
            Point::new(area.center().x, low - h / 2.0),
            &style,
            theme.text_dim,
        );
    }

    #[allow(
        clippy::many_single_char_names,
        reason = "the shared drawing names (t: transform, x/y: the pen, s1–s4: spacing steps) \
              used consistently across every draw_* method in this file"
    )]
    pub(super) fn draw_traffic_status(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        if !self.map_controls.traffic_enabled() {
            return;
        }
        if self.overlay_mode() && self.nav_open {
            return;
        }
        let ink = on_panel(theme);
        let pal = MapPalette::from_theme(theme);
        let sp = &theme.tokens.spacing;
        let (s1, s2, s3) = (f64::from(sp.s1), f64::from(sp.s2), f64::from(sp.s3));
        let left = self.map_left_edge();
        let bottom = self.overflow_rect().y1;
        let label = theme.text(TextRole::Label);
        let caption = theme.text(TextRole::Caption);
        let numeric = caption.clone().numeric();

        if self.traffic.is_empty() {
            let text = failure::t("maps-no-traffic-data");
            let text = text.as_str();
            let w = draw::measure_w(tr, text, &label) + 2.0 * s3;
            let h = self.overflow_rect().height();
            let pill = Rect::new(left, bottom - h, left + w, bottom);
            self.floats(pill);
            ui::widget::floating_surface::paint(scene, t, pill, theme);
            let mut ctx = PaintCtx::new(scene, t, tr, &ink);
            let top = draw::vmid(ctx.text, pill.y0, h, &label);
            draw::text(
                &mut ctx,
                text,
                Point::new(pill.x0 + s3, top),
                &label,
                ink.text_primary,
            );
            return;
        }

        let counts = self.traffic.counts();
        let heading = theme.text(TextRole::Subheading);
        let heading_h = draw::line_h(tr, &heading);
        let caption_h = draw::line_h(tr, &caption);
        let row_h = caption_h + s1;
        let w = f64::from(sp.s16) * 3.0;
        let h = s3 + heading_h + s2 + Congestion::ALL.len() as f64 * row_h + s2;
        let card = Rect::new(left, bottom - h, left + w, bottom);
        self.floats(card);
        ui::widget::floating_surface::paint(scene, t, card, theme);
        let mut ctx = PaintCtx::new(scene, t, tr, &ink);
        let x = card.x0 + s3;
        let right = card.x1 - s3;
        let mut y = card.y0 + s3;
        draw::text(
            &mut ctx,
            &failure::t("maps-traffic"),
            Point::new(x, y),
            &heading,
            ink.text_primary,
        );
        y += heading_h + s2;
        let swatch_w = f64::from(sp.s4);
        for level in Congestion::ALL {
            let mid = y + caption_h / 2.0;
            let swatch = Rect::new(x, mid - s1 / 2.0 - 1.0, x + swatch_w, mid + s1 / 2.0 + 1.0);
            draw::fill_round(
                ctx.scene,
                t,
                swatch,
                f64::from(theme.tokens.radii.detail),
                canvas::traffic_color(&pal, level),
            );
            let level_label = level.label();
            draw::text(
                &mut ctx,
                &level_label,
                Point::new(x + swatch_w + s2, y),
                &caption,
                ink.text_primary,
            );
            let n = counts.get(level as usize).copied().unwrap_or(0).to_string();
            draw::text_right(&mut ctx, &n, right, y, &numeric, ink.text_dim);
            y += row_h;
        }
    }
}
