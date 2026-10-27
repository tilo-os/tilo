//! The map's frame: its layers and markers, the top row and footer, the overflow menu, what it says while no imagery shows, and the traffic status it reports.

use super::*;

impl MapsApp {
    /// Whether the pointer rests on `rect` (and is not panning the map).
    pub(super) fn hovered(&self, rect: Rect) -> bool {
        !self.dragging && rect.contains(self.last_mouse)
    }

    /// Paint an icon-only button and record what it does.
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

    /// Paint a labelled button and record what it does.
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
        // Point the raster layer at the basemap style for the active theme (dark
        // tiles under a dark theme, light under light) before any tile is drawn,
        // so imagery and the drawn cartography never mix tones. A no-op on the
        // steady state; on a theme change it re-points the worker's decode.
        self.tiles.set_style(TileStyle::from_theme(theme));
        let pal = MapPalette::from_theme(theme);
        let overlay = self.overlay_mode();

        // The chrome is laid out and painted first, into its own scene, so
        // the map knows what floats over it: the camera can bring a place
        // clear of its card, and a pin or a name that would reach under the
        // panel, a card or a control is left out rather than cut at its edge.
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
        // Loading / offline affordance while the substrate grid is showing —
        // above the map but below the chrome, and skipped while the drawer
        // covers the map (the drawer is the focus then).
        if !painted_tiles && self.basemap.is_empty() && !(overlay && self.nav_open) {
            self.draw_imagery_hint(scene, t, tr, theme);
        }
        scene.append(&chrome, None);
    }

    /// The base map: real tiles or their vector/grid fallback, traffic, the
    /// route and the transit journey. Returns whether real imagery painted,
    /// which the caller needs to decide whether the offline/loading hint is due.
    pub(super) fn draw_map_layers(&mut self, ctx: &mut PaintCtx<'_>, view: MapView<'_>) -> bool {
        let MapView {
            proj, pal, clip, ..
        } = view;
        let theme = ctx.theme;
        // Map layers. Real OSM raster tiles paint first (non-blocking); the
        // stylised vector basemap is the fallback shown only until imagery is
        // available (an offline capture, or the very first offline frame).
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
                // Live first-run / offline gap: no imagery and no vector
                // cartography yet. Draw a reference grid so the map reads as a
                // map rather than a black void, with a loading affordance.
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
        // Traffic overlay: coloured congestion over the roads, under the active
        // route + markers so navigation stays legible. Empty data draws
        // nothing here — the honest "No traffic data" note is chrome.
        if self.map_controls.traffic_enabled() {
            canvas::draw_traffic(ctx.scene, ctx.transform, proj, &self.traffic, pal);
        }
        if let Some(route) = &self.model.route {
            canvas::draw_route(ctx.scene, ctx.transform, proj, route, pal);
        }
        // Public-transport itinerary: the selected journey's legs draw on the
        // map (mode-coloured lines + transfer pucks) when Transit is the active
        // mode. Only the highlighted option is drawn to keep the map legible.
        if self.model.mode.is_transit()
            && let Some(j) = self.model.transit.get(self.model.transit_choice)
        {
            canvas::draw_transit(ctx.scene, ctx.transform, proj, j, theme, pal, clip);
        }
        painted_tiles
    }

    /// The markers: the live POI layer (Overpass) — our own themed category
    /// markers, decluttered — under the person's own places and the selected
    /// pin. `view.chrome` holds what floats over the map.
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

    /// Note a floating surface drawn over the map this frame, which the
    /// markers keep clear of.
    pub(super) fn floats(&mut self, rect: Rect) {
        if rect.area() > 0.0 {
            self.chrome_rects.push(rect);
        }
    }

    /// Chrome over the map: the places panel (docked, or a drawer over a
    /// scrim when narrow), the control column, the floating cards, the
    /// footer with the scale and the credit, the overflow menu, and the
    /// tooltip on top. Modal sheets paint in
    /// the window's top layer (`draw_overlay`).
    pub(super) fn draw_chrome(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        overlay: bool,
    ) {
        let drawer_open = overlay && self.nav_open;
        // The chrome that belongs to the map: under the drawer's scrim when
        // the drawer is open.
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
            // The footer row stays above the scrim, under the drawer's end:
            // the map's credit and scale never hide behind the drawer.
            self.draw_map_footer(scene, t, tr, theme);
            self.draw_overflow(scene, t, tr, theme);
            // Stale hit and accessibility geometry must not outlive the turn
            // card the open drawer hides.
            self.navigation_drag_handle = Rect::ZERO;
            self.navigation_end_rect = Rect::ZERO;
            self.navigation_end_hovered = false;
        } else {
            self.draw_traffic_status(scene, t, tr, theme);
            // The place card floats over the map, its actions above the
            // controls in the hit-test.
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
            // The Directions From/To suggestions float under the focused
            // field so the panel below never reflows.
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

    /// The overflow button (attribution, offline maps, licences) in the
    /// bottom-trailing corner; pressed while its menu is open.
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
            (Icon::MoreHorizontal, "More"),
            ButtonRole::Secondary,
            open,
            Hit::ToggleOverflow,
        );
    }

    /// The narrow window's top row: the button that shows and hides the
    /// places drawer, and the search button beside it. A docked panel
    /// carries search in its own header instead.
    pub(super) fn draw_top_row(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let Some(menu) = self.menu_rect() else { return };
        let name = if self.nav_open {
            "Hide places"
        } else {
            "Show places"
        };
        let open = self.nav_open;
        self.icon_button(
            &mut PaintCtx::new(scene, t, tr, theme),
            menu,
            (Icon::PanelLeft, name),
            ButtonRole::Secondary,
            open,
            Hit::ToggleNav,
        );
        if let Some(search) = self.search_button_rect() {
            self.icon_button(
                &mut PaintCtx::new(scene, t, tr, theme),
                search,
                (Icon::Search, "Search places"),
                ButtonRole::Secondary,
                false,
                Hit::OpenSearch,
            );
            self.floats(menu.union(search));
        }
    }

    /// The scale and the tile credit, on one floating pill left of the
    /// overflow button: the credit, then the scale's label with its bar a
    /// token gap after it, so the label never sits on the line. When the map
    /// is too narrow for both, the credit floats on its own pill above.
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
        // The bar itself is map content: a line the length of the labelled
        // distance with a tick at each end.
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

    /// The model's status in the window's bar: a new line is news; one
    /// that ends in "…" is work under way and stays while it is the status.
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

    /// Text over the substrate grid telling the person the imagery is
    /// loading (or unreachable), so a first-run / offline map is never a
    /// silent void. Text only, centred low in the visible map area: the
    /// centre is where the chosen place and its pin are drawn.
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
            msg,
            Point::new(area.center().x, low - h / 2.0),
            &style,
            theme.text_dim,
        );
    }

    /// The traffic key, bottom-leading in the map area: with data, the
    /// congestion levels with their counts; without, the honest "No traffic
    /// data". Nothing while the overlay is off.
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
        // While the narrow drawer is open the map is dimmed and the drawer is
        // the focus — don't stack a key over it.
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
            let text = "No traffic data";
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
            "Traffic",
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
            draw::text(
                &mut ctx,
                level.label(),
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
