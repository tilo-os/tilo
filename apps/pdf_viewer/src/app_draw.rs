// Included into `app.rs` (shares its module + imports). Holds the chrome
// drawing routines and the pure layout helpers that both `draw` and event
// hit-testing rely on, so a click always lands where the pixel is.

struct ToolbarLayout {
    tools: Vec<(Tool, Rect)>,
    swatches: Vec<(Swatch, Rect)>,
    zoom_out: Rect,
    zoom_label: Rect,
    zoom_in: Rect,
    fit: Rect,
    search: Rect,
    toggle_rail: Rect,
    toggle_sidebar: Rect,
}

fn vcenter(rect: Rect, size: f32) -> f64 {
    rect.y0 + (rect.height() - size as f64) / 2.0
}

impl PdfApp {
    // ---- pure layout ------------------------------------------------

    fn toolbar_layout(&self) -> ToolbarLayout {
        let tb = self.toolbar_rect;
        let cy = (tb.y0 + tb.y1) / 2.0;
        let bw = 34.0;
        let bh = 34.0;
        let gap = 6.0;
        let n = Tool::ALL.len() as f64;
        let tools_w = n * bw + (n - 1.0) * gap;
        let center_x = (tb.x0 + tb.x1) / 2.0;
        let mut tx = center_x - tools_w / 2.0;
        let mut tools = Vec::new();
        for t in Tool::ALL {
            tools.push((t, Rect::new(tx, cy - bh / 2.0, tx + bw, cy + bh / 2.0)));
            tx += bw + gap;
        }

        // Swatches sit just right of the tool cluster (only meaningful for
        // the annotate tools, but laid out unconditionally so hit-testing
        // is stable).
        let mut swatches = Vec::new();
        let sw = 16.0;
        let sgap = 8.0;
        let mut sx = tx + 14.0;
        for s in Swatch::ALL {
            swatches.push((s, Rect::new(sx, cy - sw / 2.0, sx + sw, cy + sw / 2.0)));
            sx += sw + sgap;
        }

        // Right cluster, anchored to the right edge.
        let zw = 28.0;
        let zlabel = 52.0;
        let fitw = 40.0;
        let total = zw + zlabel + zw + 10.0 + fitw + 16.0 + bw + 8.0 + bw + 8.0 + bw;
        let mut rx = tb.x1 - 16.0 - total;
        let mk = |x: &mut f64, w: f64| {
            let r = Rect::new(*x, cy - bh / 2.0, *x + w, cy + bh / 2.0);
            *x += w;
            r
        };
        let zoom_out = mk(&mut rx, zw);
        let zoom_label = mk(&mut rx, zlabel);
        let zoom_in = mk(&mut rx, zw);
        rx += 10.0;
        let fit = mk(&mut rx, fitw);
        rx += 16.0;
        let search = mk(&mut rx, bw);
        rx += 8.0;
        let toggle_rail = mk(&mut rx, bw);
        rx += 8.0;
        let toggle_sidebar = mk(&mut rx, bw);

        ToolbarLayout {
            tools,
            swatches,
            zoom_out,
            zoom_label,
            zoom_in,
            fit,
            search,
            toggle_rail,
            toggle_sidebar,
        }
    }

    /// Every tooltip-able toolbar button as `(label, anchor-rect)`. Used both
    /// to draw the hover popover and to resolve which button is under the
    /// pointer. Labels name the tool/action and its keyboard shortcut.
    fn toolbar_tooltips(&self) -> Vec<(String, Rect)> {
        let lay = self.toolbar_layout();
        let mut out: Vec<(String, Rect)> = Vec::new();
        for (t, r) in &lay.tools {
            out.push((format!("{}  ({})", t.label(), t.hotkey()), *r));
        }
        if matches!(self.tool, Tool::Highlight | Tool::Ink | Tool::Note) {
            for (s, r) in &lay.swatches {
                out.push((format!("{} marker", swatch_label(*s)), *r));
            }
        }
        out.push(("Zoom out  (Ctrl −)".to_string(), lay.zoom_out));
        out.push(("Zoom in  (Ctrl +)".to_string(), lay.zoom_in));
        out.push(("Reset zoom / fit  (Ctrl 0)".to_string(), lay.zoom_label));
        out.push((
            match self.fit {
                FitMode::Width => "Fit width  (W)".to_string(),
                _ => "Fit page  (0)".to_string(),
            },
            lay.fit,
        ));
        out.push(("Find in document  (Ctrl F)".to_string(), lay.search));
        out.push((
            if self.show_rail { "Hide page thumbnails".to_string() } else { "Show page thumbnails".to_string() },
            lay.toggle_rail,
        ));
        out.push((
            if self.show_sidebar { "Hide sidebar  (B)".to_string() } else { "Show sidebar  (B)".to_string() },
            lay.toggle_sidebar,
        ));
        out
    }

    /// Resolve the tooltip for the current pointer position and advance its
    /// reveal timing. Called once per frame from the toolbar draw so the
    /// popover appears after the configured hover delay.
    fn resolve_tooltip(&mut self) {
        let hovered = self
            .toolbar_tooltips()
            .into_iter()
            .find(|(_, r)| r.contains(self.last_mouse));
        match hovered {
            Some((label, anchor)) => {
                if self.tooltip.label() != label {
                    // Fresh anchor — rebuild and restart the hover timer.
                    self.tooltip = Tooltip::new(label).with_delay(std::time::Duration::from_millis(350));
                    self.tooltip.on_hover_enter(std::time::Duration::ZERO);
                    self.tooltip_anchor = anchor;
                } else {
                    self.tooltip_anchor = anchor;
                    // Advance time so the reveal delay can elapse across frames.
                    self.tooltip.tick(std::time::Duration::from_millis(120));
                }
            }
            None => {
                if self.tooltip.label() != "" {
                    self.tooltip.on_hover_leave();
                    self.tooltip = Tooltip::new("").with_delay(std::time::Duration::from_millis(350));
                }
            }
        }
    }

    fn status_layout(&self) -> (Rect, Rect, Rect) {
        let s = self.status_rect;
        let cy = (s.y0 + s.y1) / 2.0;
        let bw = 22.0;
        let label_w = 132.0;
        let total = bw + label_w + bw;
        let cx = (s.x0 + s.x1) / 2.0;
        let mut x = cx - total / 2.0;
        let prev = Rect::new(x, cy - 9.0, x + bw, cy + 9.0);
        x += bw;
        let label = Rect::new(x, s.y0, x + label_w, s.y1);
        x += label_w;
        let next = Rect::new(x, cy - 9.0, x + bw, cy + 9.0);
        (prev, label, next)
    }

    fn sidebar_tab_rects(&self) -> [(SidebarTab, Rect); 3] {
        let sb = self.sidebar_rect;
        let h = 38.0;
        let w = sb.width() / 3.0;
        [
            (SidebarTab::Comments, Rect::new(sb.x0, sb.y0, sb.x0 + w, sb.y0 + h)),
            (SidebarTab::Outline, Rect::new(sb.x0 + w, sb.y0, sb.x0 + 2.0 * w, sb.y0 + h)),
            (SidebarTab::Signatures, Rect::new(sb.x0 + 2.0 * w, sb.y0, sb.x1, sb.y0 + h)),
        ]
    }

    fn thumbnail_rects(&self) -> Vec<(usize, Rect)> {
        let rail = self.rail_rect;
        let n = self.doc.page_count();
        if n == 0 {
            return vec![];
        }
        let pad = 12.0;
        let label_h = 14.0;
        let gap = 10.0;
        let avail = rail.height() - 2.0 * pad;
        let slot = (avail - (n as f64 - 1.0) * gap) / n as f64;
        let thumb_h = (slot - label_h).clamp(44.0, 124.0);
        let thumb_w = thumb_h * PAGE_W / PAGE_H;
        let cx = (rail.x0 + rail.x1) / 2.0;
        let mut y = rail.y0 + pad;
        let mut out = Vec::new();
        for i in 0..n {
            let r = Rect::new(cx - thumb_w / 2.0, y, cx + thumb_w / 2.0, y + thumb_h);
            out.push((i + 1, r));
            y += thumb_h + label_h + gap;
        }
        out
    }

    fn pages_with_markup(&self) -> HashSet<usize> {
        let mut set = HashSet::new();
        for a in &self.annotations {
            set.insert(a.page);
        }
        for s in &self.signatures {
            set.insert(s.page);
        }
        set
    }

    // ---- orchestration ---------------------------------------------

    fn draw_all(&mut self, scene: &mut Scene, base: Affine, tr: &mut TextRenderer, theme: &Theme) {
        self.current_page = self.nearest_page();
        let c = &theme.tokens.colors;

        // Whole-window backdrop.
        scene.fill(Fill::NonZero, base, c.surfaces.base, None, &self.rect);

        // Refresh the page-point text-line cache for visible pages so the
        // smart highlight tool can snap to exactly the glyphs being drawn.
        self.refresh_text_lines(tr, theme);

        // Resolve which toolbar button (if any) the pointer is hovering and
        // advance the tooltip reveal timer before the toolbar paints.
        self.resolve_tooltip();

        // Resolve sidebar scroll + any reveal request before painting it.
        self.prepare_sidebar_scroll(tr);

        self.draw_canvas(scene, base, tr, theme);
        self.draw_rail(scene, base, tr, theme);
        let hits = self.draw_sidebar(scene, base, tr, theme);
        self.draw_toolbar(scene, base, tr, theme);
        if self.search_active {
            let shits = self.draw_search(scene, base, tr, theme);
            self.search_hits = shits;
        } else {
            self.search_hits.clear();
        }
        self.draw_status(scene, base, tr, theme);
        // Context menu + tooltip composite above everything else.
        self.draw_context_menu(scene, base, tr, theme);
        self.draw_tooltip(scene, base, tr, theme);

        self.sidebar_hits = hits;
    }

    /// Populate `self.text_lines` for the pages currently in view. Cheap: the
    /// measurer caches per (text,size,width) so repeated frames hit the cache.
    fn refresh_text_lines(&mut self, tr: &mut TextRenderer, theme: &Theme) {
        let pageh = PAGE_H * self.zoom;
        let canvas = self.canvas_rect;
        let origins = self.page_origins();
        self.text_lines.clear();
        for (i, origin) in origins.iter().enumerate() {
            if origin.y > canvas.y1 || origin.y + pageh < canvas.y0 {
                continue;
            }
            let page_num = i + 1;
            if let Some(page) = self.doc.page(page_num) {
                let lines = render::collect_text_lines(tr, theme, page);
                self.text_lines.insert(page_num, lines);
            }
        }
    }

    fn draw_tooltip(&mut self, scene: &mut Scene, base: Affine, tr: &mut TextRenderer, theme: &Theme) {
        if !self.tooltip.is_visible() || self.tooltip.label().is_empty() {
            return;
        }
        let label = self.tooltip.label().to_string();
        let anchor = self.tooltip_anchor;
        let size = theme.tokens.typography.scale.sm;
        let (tw, _) = tr.measure(&label, size, None);
        let pad_x = 10.0;
        let w = tw as f64 + 2.0 * pad_x;
        let h = 26.0;
        // Below the button, clamped into the window.
        let mut x = (anchor.x0 + anchor.x1) / 2.0 - w / 2.0;
        x = x.clamp(self.rect.x0 + 4.0, self.rect.x1 - w - 4.0);
        let y = (anchor.y1 + 6.0).min(self.rect.y1 - h - 4.0);
        let r = Rect::new(x, y, x + w, y + h);
        let shape = RoundedRect::from_rect(r, theme.tokens.radii.sm as f64);
        scene.fill(Fill::NonZero, base, theme.tokens.colors.surfaces.raised, None, &shape);
        scene.stroke(&Stroke::new(1.0), base, theme.tokens.colors.border_standard, None, &shape);
        tr.draw_text(
            scene,
            &label,
            size,
            theme.tokens.colors.text_primary,
            base * Affine::translate((x + pad_x, vcenter(r, size))),
            None,
        );
    }

    fn draw_context_menu(&mut self, scene: &mut Scene, base: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let Some(menu) = self.context_menu.clone() else { return };
        let c = &theme.tokens.colors;
        let w = 150.0;
        let item_h = 30.0;
        let h = item_h + 8.0;
        let mut x = menu.at.x;
        let mut y = menu.at.y;
        x = x.clamp(self.rect.x0 + 2.0, self.rect.x1 - w - 2.0);
        y = y.clamp(self.rect.y0 + 2.0, self.rect.y1 - h - 2.0);
        let panel = Rect::new(x, y, x + w, y + h);
        let shape = RoundedRect::from_rect(panel, theme.tokens.radii.md as f64);
        // Soft shadow (a slightly-offset translucent slab behind the panel).
        let sh = RoundedRect::new(panel.x0, panel.y0 + 3.0, panel.x1, panel.y1 + 3.0, theme.tokens.radii.md as f64);
        scene.fill(Fill::NonZero, base, Color::from_rgba8(0, 0, 0, 60), None, &sh);
        scene.fill(Fill::NonZero, base, c.surfaces.raised, None, &shape);
        scene.stroke(&Stroke::new(1.0), base, c.border_standard, None, &shape);

        let item = Rect::new(panel.x0 + 4.0, panel.y0 + 4.0, panel.x1 - 4.0, panel.y0 + 4.0 + item_h);
        if item.contains(self.last_mouse) {
            scene.fill(Fill::NonZero, base, c.surface_white_10, None, &RoundedRect::from_rect(item, theme.tokens.radii.sm as f64));
        }
        // Trash glyph + label.
        let gx = item.x0 + 14.0;
        let gy = (item.y0 + item.y1) / 2.0;
        let lid = Line::new((gx - 5.0, gy - 5.0), (gx + 5.0, gy - 5.0));
        scene.stroke(&Stroke::new(1.4), base, c.semantic.danger, None, &lid);
        let body = Rect::new(gx - 4.0, gy - 4.0, gx + 4.0, gy + 6.0);
        scene.stroke(&Stroke::new(1.4), base, c.semantic.danger, None, &RoundedRect::from_rect(body, 1.5));
        tr.draw_text_weighted(
            scene,
            "Delete",
            12.0,
            c.semantic.danger,
            base * Affine::translate((gx + 16.0, vcenter(item, 12.0))),
            None,
            TextWeight::Semibold,
        );
        // Stash the live item rect so the click handler can hit it.
        self.context_menu = Some(ContextMenu { item_rect: item, ..menu });
    }

    // ---- canvas -----------------------------------------------------

    fn draw_canvas(&self, scene: &mut Scene, base: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let canvas = self.canvas_rect;
        scene.fill(Fill::NonZero, base, theme.tokens.colors.surfaces.base, None, &canvas);
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, base, &canvas);

        let pageh = PAGE_H * self.zoom;
        let origins = self.page_origins();
        let markup = self.pages_with_markup();
        let _ = &markup;
        for (i, origin) in origins.iter().enumerate() {
            let page_num = i + 1;
            // Vertical cull.
            if origin.y > canvas.y1 || origin.y + pageh < canvas.y0 {
                continue;
            }
            let Some(page) = self.doc.page(page_num) else { continue };
            let (matched, active) = self.matched_blocks_for(page_num);
            render::draw_page(
                scene,
                tr,
                theme,
                base,
                page,
                *origin,
                self.zoom,
                &matched,
                active,
                &self.annotations,
                &self.signatures,
                self.selected_anno.as_deref(),
            );
        }

        // Drag preview.
        if let Some(d) = &self.drag {
            if let Some(origin) = origins.get(d.page.saturating_sub(1)) {
                let col = render::swatch_color(theme, self.swatch);
                match d.tool {
                    Tool::Highlight => {
                        let r = PtRect::from_corners(d.start.0, d.start.1, d.cur.0, d.cur.1);
                        let scr = Rect::new(
                            origin.x + r.x * self.zoom,
                            origin.y + r.y * self.zoom,
                            origin.x + (r.x + r.w) * self.zoom,
                            origin.y + (r.y + r.h) * self.zoom,
                        );
                        scene.fill(Fill::NonZero, base, render::alpha(col, 80), None, &scr);
                        scene.stroke(&Stroke::new(1.0), base, col, None, &scr);
                    }
                    Tool::Ink => {
                        if d.points.len() >= 2 {
                            let mut path = BezPath::new();
                            let p0 = Point::new(origin.x + d.points[0].0 * self.zoom, origin.y + d.points[0].1 * self.zoom);
                            path.move_to(p0);
                            for p in &d.points[1..] {
                                path.line_to((origin.x + p.0 * self.zoom, origin.y + p.1 * self.zoom));
                            }
                            scene.stroke(&Stroke::new((2.4 * self.zoom).max(1.5)), base, col, None, &path);
                        }
                    }
                    _ => {}
                }
            }
        }

        scene.pop_layer();

        // Slim vertical scrollbar.
        let max = self.max_scroll();
        if max > 1.0 {
            let track_h = canvas.height();
            let frac = (canvas.height() / self.content_height()).clamp(0.06, 1.0);
            let thumb_h = track_h * frac;
            let t = self.scroll_y / max;
            let ty = canvas.y0 + t * (track_h - thumb_h);
            let bar = RoundedRect::new(canvas.x1 - 7.0, ty + 2.0, canvas.x1 - 3.0, ty + thumb_h - 2.0, 2.0);
            scene.fill(Fill::NonZero, base, theme.tokens.colors.surface_white_20, None, &bar);
        }

        // Horizontal scrollbar — appears only when the zoomed page is wider
        // than the viewport, and spans the full reachable width.
        let maxx = self.max_scroll_x();
        if maxx > 1.0 {
            let track_w = canvas.width();
            let frac = (canvas.width() / self.content_width()).clamp(0.06, 1.0);
            let thumb_w = track_w * frac;
            let t = self.scroll_x / maxx;
            let tx = canvas.x0 + t * (track_w - thumb_w);
            let bar = RoundedRect::new(tx + 2.0, canvas.y1 - 7.0, tx + thumb_w - 2.0, canvas.y1 - 3.0, 2.0);
            scene.fill(Fill::NonZero, base, theme.tokens.colors.surface_white_20, None, &bar);
        }
    }

    // ---- thumbnail rail --------------------------------------------

    fn draw_rail(&self, scene: &mut Scene, base: Affine, tr: &mut TextRenderer, theme: &Theme) {
        if !self.show_rail {
            return;
        }
        let rail = self.rail_rect;
        let c = &theme.tokens.colors;
        scene.fill(Fill::NonZero, base, c.surfaces.raised, None, &rail);
        // Right divider doubles as the resize grip; brighten it while the
        // pointer is over the grip strip or actively dragging it.
        let grip_hot = self.rail_drag.is_some()
            || (self.last_mouse.x >= rail.x1 - RAIL_GRIP
                && self.last_mouse.x <= rail.x1 + RAIL_GRIP
                && self.last_mouse.y >= rail.y0
                && self.last_mouse.y <= rail.y1);
        let div_col = if grip_hot { theme.accent_primary } else { c.border_standard };
        let div_w = if grip_hot { 2.0 } else { 1.0 };
        let div = Rect::new(rail.x1 - div_w, rail.y0, rail.x1, rail.y1);
        scene.fill(Fill::NonZero, base, div_col, None, &div);

        let markup = self.pages_with_markup();
        for (num, r) in self.thumbnail_rects() {
            if r.y0 > rail.y1 || r.y1 < rail.y0 {
                continue;
            }
            if let Some(page) = self.doc.page(num) {
                render::draw_thumbnail(scene, theme, base, page, r, num == self.current_page, markup.contains(&num));
            }
            let label = format!("{}", num);
            let (lw, _) = tr.measure(&label, 10.0, None);
            let lx = (r.x0 + r.x1) / 2.0 - lw as f64 / 2.0;
            let col = if num == self.current_page { theme.text_primary } else { theme.text_dim };
            tr.draw_text_weighted(
                scene,
                &label,
                10.0,
                col,
                base * Affine::translate((lx, r.y1 + 2.0)),
                None,
                if num == self.current_page { TextWeight::Semibold } else { TextWeight::Regular },
            );
        }
    }

    // ---- toolbar ----------------------------------------------------

    fn draw_toolbar(&self, scene: &mut Scene, base: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let tb = self.toolbar_rect;
        let c = &theme.tokens.colors;
        scene.fill(Fill::NonZero, base, c.surfaces.raised, None, &tb);
        let hair = Rect::new(tb.x0, tb.y1 - 1.0, tb.x1, tb.y1);
        scene.fill(Fill::NonZero, base, c.border_standard, None, &hair);

        // Wordmark: a little document glyph + name + doc title.
        let cy = (tb.y0 + tb.y1) / 2.0;
        let gx = tb.x0 + 16.0;
        let glyph = Rect::new(gx, cy - 11.0, gx + 17.0, cy + 11.0);
        scene.fill(Fill::NonZero, base, theme.accent_primary, None, &RoundedRect::from_rect(glyph, 3.0));
        let fold = {
            let mut p = BezPath::new();
            p.move_to((glyph.x1 - 6.0, glyph.y0));
            p.line_to((glyph.x1, glyph.y0 + 6.0));
            p.line_to((glyph.x1 - 6.0, glyph.y0 + 6.0));
            p.close_path();
            p
        };
        scene.fill(Fill::NonZero, base, Color::from_rgba8(255, 255, 255, 120), None, &fold);
        for i in 0..3 {
            let ly = glyph.y0 + 8.0 + i as f64 * 4.0;
            let line = Line::new((glyph.x0 + 4.0, ly), (glyph.x1 - 4.0, ly));
            scene.stroke(&Stroke::new(1.0), base, Color::from_rgba8(255, 255, 255, 200), None, &line);
        }
        tr.draw_text_weighted(
            scene,
            "PDF",
            13.0,
            theme.text_primary,
            base * Affine::translate((gx + 24.0, vcenter(tb, 13.0) - 6.0)),
            None,
            TextWeight::Bold,
        );
        tr.draw_text(
            scene,
            &self.doc.title,
            10.0,
            theme.text_dim,
            base * Affine::translate((gx + 24.0, vcenter(tb, 10.0) + 7.0)),
            None,
        );

        let lay = self.toolbar_layout();
        let annotate = matches!(self.tool, Tool::Highlight | Tool::Ink | Tool::Note);

        for (t, r) in &lay.tools {
            let active = *t == self.tool;
            let hovered = r.contains(self.last_mouse);
            self.icon_button_bg(scene, base, theme, *r, active, hovered);
            let col = if active { Color::WHITE } else { theme.text_dim };
            draw_tool_icon(scene, base, *t, *r, col);
        }

        if annotate {
            for (s, r) in &lay.swatches {
                let col = render::swatch_color(theme, *s);
                let center = Point::new((r.x0 + r.x1) / 2.0, (r.y0 + r.y1) / 2.0);
                let active = *s == self.swatch;
                let rad = if active { 8.0 } else { 6.5 };
                scene.fill(Fill::NonZero, base, col, None, &Circle::new(center, rad));
                if active {
                    scene.stroke(&Stroke::new(2.0), base, theme.text_primary, None, &Circle::new(center, rad + 2.5));
                }
            }
        }

        // Zoom group.
        self.icon_button_bg(scene, base, theme, lay.zoom_out, false, lay.zoom_out.contains(self.last_mouse));
        draw_minus(scene, base, lay.zoom_out, theme.text_primary);
        self.icon_button_bg(scene, base, theme, lay.zoom_in, false, lay.zoom_in.contains(self.last_mouse));
        draw_plus(scene, base, lay.zoom_in, theme.text_primary);
        let zlabel = format!("{}%", (self.zoom * 100.0).round() as i64);
        let (zw, _) = tr.measure(&zlabel, 12.0, None);
        tr.draw_text(
            scene,
            &zlabel,
            12.0,
            theme.text_primary,
            base * Affine::translate(((lay.zoom_label.x0 + lay.zoom_label.x1) / 2.0 - zw as f64 / 2.0, vcenter(lay.zoom_label, 12.0))),
            None,
        );

        // Fit button.
        let fit_active = !matches!(self.fit, FitMode::Custom(_));
        self.icon_button_bg(scene, base, theme, lay.fit, fit_active, lay.fit.contains(self.last_mouse));
        let fit_txt = match self.fit {
            FitMode::Width => "Wide",
            _ => "Fit",
        };
        let (fw, _) = tr.measure(fit_txt, 11.0, None);
        tr.draw_text(
            scene,
            fit_txt,
            11.0,
            if fit_active { Color::WHITE } else { theme.text_dim },
            base * Affine::translate(((lay.fit.x0 + lay.fit.x1) / 2.0 - fw as f64 / 2.0, vcenter(lay.fit, 11.0))),
            None,
        );

        // Search, rail toggle, sidebar toggle.
        self.icon_button_bg(scene, base, theme, lay.search, self.search_active, lay.search.contains(self.last_mouse));
        draw_search_glyph(scene, base, lay.search, if self.search_active { Color::WHITE } else { theme.text_dim });
        self.icon_button_bg(scene, base, theme, lay.toggle_rail, self.show_rail, lay.toggle_rail.contains(self.last_mouse));
        draw_rail_glyph(scene, base, lay.toggle_rail, if self.show_rail { Color::WHITE } else { theme.text_dim });
        self.icon_button_bg(scene, base, theme, lay.toggle_sidebar, self.show_sidebar, lay.toggle_sidebar.contains(self.last_mouse));
        draw_panel_glyph(scene, base, lay.toggle_sidebar, if self.show_sidebar { Color::WHITE } else { theme.text_dim });
    }

    fn icon_button_bg(&self, scene: &mut Scene, base: Affine, theme: &Theme, r: Rect, active: bool, hovered: bool) {
        let shape = RoundedRect::from_rect(r.inflate(-2.0, -2.0), theme.tokens.radii.md as f64);
        if active {
            scene.fill(Fill::NonZero, base, theme.accent_primary, None, &shape);
        } else if hovered {
            scene.fill(Fill::NonZero, base, theme.tokens.colors.surface_white_10, None, &shape);
        }
    }

    // ---- search strip ----------------------------------------------

    fn draw_search(&self, scene: &mut Scene, base: Affine, tr: &mut TextRenderer, theme: &Theme) -> Vec<(SearchHit, Rect)> {
        let mut hits: Vec<(SearchHit, Rect)> = Vec::new();
        let s = self.search_rect;
        let c = &theme.tokens.colors;
        scene.fill(Fill::NonZero, base, c.surfaces.glass, None, &s);
        let hair = Rect::new(s.x0, s.y1 - 1.0, s.x1, s.y1);
        scene.fill(Fill::NonZero, base, c.border_standard, None, &hair);

        let field = Rect::new(s.x0 + 16.0, s.y0 + 8.0, s.x0 + 460.0, s.y1 - 8.0);
        scene.fill(Fill::NonZero, base, c.surfaces.base, None, &RoundedRect::from_rect(field, theme.tokens.radii.md as f64));
        scene.stroke(&Stroke::new(1.5), base, theme.border_focus, None, &RoundedRect::from_rect(field, theme.tokens.radii.md as f64));
        draw_search_glyph(scene, base, Rect::new(field.x0, field.y0, field.x0 + 30.0, field.y1), theme.text_dim);

        let shown = if self.query.is_empty() { "Search document…".to_string() } else { self.query.clone() };
        let col = if self.query.is_empty() { theme.text_dim } else { theme.text_primary };
        tr.draw_text(
            scene,
            &shown,
            13.0,
            col,
            base * Affine::translate((field.x0 + 32.0, vcenter(field, 13.0))),
            Some((field.width() - 40.0) as f32),
        );
        // blinking-less caret indicator after text
        if !self.query.is_empty() {
            let (qw, _) = tr.measure(&self.query, 13.0, None);
            let cx = field.x0 + 32.0 + qw as f64 + 1.0;
            let caret = Rect::new(cx, field.y0 + 6.0, cx + 1.5, field.y1 - 6.0);
            scene.fill(Fill::NonZero, base, theme.accent_primary, None, &caret);
        }

        // Match counter.
        let counter = if self.matches.is_empty() {
            if self.query.is_empty() { String::new() } else { "No matches".to_string() }
        } else {
            format!("{} of {} matches", self.active_match + 1, self.matches.len())
        };
        tr.draw_text(
            scene,
            &counter,
            12.0,
            theme.text_dim,
            base * Affine::translate((field.x1 + 18.0, vcenter(s, 12.0))),
            None,
        );

        // Find-bar option toggles: Whole word + Case sensitive. Each is a
        // pill the user clicks to flip; the matcher honours both live.
        let chip_h = 22.0;
        let chip_y = (s.y0 + s.y1) / 2.0 - chip_h / 2.0;
        let mk_chip = |label: &str, right: f64, tr: &mut TextRenderer| -> (Rect, f64) {
            let (tw, _) = tr.measure(label, 11.0, None);
            let w = tw as f64 + 22.0;
            let r = Rect::new(right - w, chip_y, right, chip_y + chip_h);
            (r, w)
        };
        let hint = "Enter ▸ next  ·  Esc ▸ close";
        let (hw, _) = tr.measure(hint, 10.5, None);
        let mut right = s.x1 - 16.0 - hw as f64 - 14.0;

        for (label, active, hit) in [
            ("Case", self.search_case_sensitive, SearchHit::ToggleCaseSensitive),
            ("Whole word", self.search_whole_word, SearchHit::ToggleWholeWord),
        ] {
            let (r, w) = mk_chip(label, right, tr);
            let fill = if active { theme.accent_primary } else { c.surface_white_10 };
            let txt = if active { Color::WHITE } else { theme.text_dim };
            scene.fill(Fill::NonZero, base, fill, None, &RoundedRect::from_rect(r, theme.tokens.radii.full as f64));
            let (lw, _) = tr.measure(label, 11.0, None);
            tr.draw_text(
                scene,
                label,
                11.0,
                txt,
                base * Affine::translate(((r.x0 + r.x1) / 2.0 - lw as f64 / 2.0, vcenter(r, 11.0))),
                None,
            );
            hits.push((hit, r));
            right -= w + 8.0;
        }

        tr.draw_text(
            scene,
            hint,
            10.5,
            theme.text_dim,
            base * Affine::translate((s.x1 - 16.0 - hw as f64, vcenter(s, 10.5))),
            None,
        );
        hits
    }

    // ---- sidebar ----------------------------------------------------

    fn draw_sidebar(&self, scene: &mut Scene, base: Affine, tr: &mut TextRenderer, theme: &Theme) -> Vec<(SidebarHit, Rect)> {
        let mut hits = Vec::new();
        if !self.show_sidebar {
            return hits;
        }
        let sb = self.sidebar_rect;
        let c = &theme.tokens.colors;
        scene.fill(Fill::NonZero, base, c.surfaces.raised, None, &sb);
        let div = Rect::new(sb.x0, sb.y0, sb.x0 + 1.0, sb.y1);
        scene.fill(Fill::NonZero, base, c.border_standard, None, &div);

        // Tabs.
        for (tab, r) in self.sidebar_tab_rects() {
            let active = tab == self.sidebar_tab;
            let label = match tab {
                SidebarTab::Comments => "Comments",
                SidebarTab::Outline => "Contents",
                SidebarTab::Signatures => "Signatures",
            };
            let (lw, _) = tr.measure(label, 11.5, None);
            let col = if active { theme.text_primary } else { theme.text_dim };
            tr.draw_text_weighted(
                scene,
                label,
                11.5,
                col,
                base * Affine::translate(((r.x0 + r.x1) / 2.0 - lw as f64 / 2.0, vcenter(r, 11.5))),
                None,
                if active { TextWeight::Semibold } else { TextWeight::Regular },
            );
            if active {
                let underline = Rect::new(r.x0 + 12.0, r.y1 - 2.0, r.x1 - 12.0, r.y1);
                scene.fill(Fill::NonZero, base, theme.accent_primary, None, &underline);
            }
            hits.push((SidebarHit::Tab(tab), r));
        }
        let tabs_hair = Rect::new(sb.x0, sb.y0 + 38.0 - 1.0, sb.x1, sb.y0 + 38.0);
        scene.fill(Fill::NonZero, base, c.border_standard, None, &tabs_hair);

        let body = Rect::new(sb.x0, sb.y0 + 38.0, sb.x1, sb.y1);
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, base, &body);
        match self.sidebar_tab {
            SidebarTab::Comments => self.draw_comments(scene, base, tr, theme, body, &mut hits),
            SidebarTab::Outline => self.draw_outline(scene, base, tr, theme, body, &mut hits),
            SidebarTab::Signatures => self.draw_signatures(scene, base, tr, theme, body, &mut hits),
        }
        scene.pop_layer();
        hits
    }

    /// The scrollable body rect of the sidebar (below the tab strip).
    fn sidebar_body(&self) -> Rect {
        let sb = self.sidebar_rect;
        Rect::new(sb.x0, sb.y0 + 38.0, sb.x1, sb.y1)
    }

    /// Content-relative layout of the active sidebar list as
    /// `(id, top, height)` entries plus the total content height. Tops are
    /// measured from the same origin the draw routines use *before* the
    /// scroll offset is applied, so the reveal math lines up exactly.
    fn sidebar_item_layout(&self, tr: &mut TextRenderer) -> (Vec<(String, f64, f64)>, f64) {
        let body = self.sidebar_body();
        let pad = 14.0;
        let inner_w = body.width() - 2.0 * pad;
        let mut items = Vec::new();
        match self.sidebar_tab {
            SidebarTab::Comments => {
                let mut y = body.y0 + 12.0 + 22.0;
                for cm in &self.comments {
                    let (_, bh) = tr.measure(&cm.body, 12.0, Some(inner_w as f32 - 16.0));
                    let mut card_h = 18.0 + 6.0 + bh as f64 + 12.0;
                    for r in &cm.replies {
                        let (_, rh) = tr.measure(&r.body, 11.0, Some(inner_w as f32 - 36.0));
                        card_h += 16.0 + rh as f64 + 8.0;
                    }
                    items.push((cm.id.clone(), y, card_h));
                    y += card_h + 12.0;
                }
                (items, y - (body.y0 + 12.0 + 22.0))
            }
            SidebarTab::Signatures => {
                // Signer card (70) + add button (52) + header (20) are fixed;
                // only the placed rows scroll.
                let fixed = 70.0 + 52.0 + 20.0;
                let mut y = body.y0 + 12.0 + fixed;
                for s in &self.signatures {
                    items.push((s.id.clone(), y, 46.0));
                    y += 56.0;
                }
                (items, y - (body.y0 + 12.0 + fixed))
            }
            SidebarTab::Outline => (items, 0.0),
        }
    }

    /// Resolve `sidebar_scroll`: honour a pending reveal request (scrolling a
    /// freshly created comment/signature into view) and clamp to the list's
    /// scroll range so a deleted tail can't leave the list scrolled off.
    fn prepare_sidebar_scroll(&mut self, tr: &mut TextRenderer) {
        if !self.show_sidebar || self.sidebar_tab == SidebarTab::Outline {
            self.sidebar_scroll = 0.0;
            self.sidebar_reveal = None;
            return;
        }
        let body = self.sidebar_body();
        let viewport = (body.height() - 24.0).max(0.0);
        let (items, content_h) = self.sidebar_item_layout(tr);
        let max_scroll = (content_h - viewport).max(0.0);

        if let Some(id) = self.sidebar_reveal.take() {
            if let Some((_, top, h)) = items.iter().find(|(iid, _, _)| iid == &id) {
                // `top` is content-relative-from-list-origin; bring it just
                // inside the viewport.
                let list_origin = match self.sidebar_tab {
                    SidebarTab::Comments => body.y0 + 12.0 + 22.0,
                    SidebarTab::Signatures => body.y0 + 12.0 + 70.0 + 52.0 + 20.0,
                    SidebarTab::Outline => body.y0,
                };
                let rel_top = top - list_origin;
                let rel_bot = rel_top + h;
                if rel_top < self.sidebar_scroll {
                    self.sidebar_scroll = rel_top;
                } else if rel_bot > self.sidebar_scroll + viewport {
                    self.sidebar_scroll = rel_bot - viewport;
                }
            }
        }
        self.sidebar_scroll = self.sidebar_scroll.clamp(0.0, max_scroll);
    }

    fn draw_comments(&self, scene: &mut Scene, base: Affine, tr: &mut TextRenderer, theme: &Theme, body: Rect, hits: &mut Vec<(SidebarHit, Rect)>) {
        let c = &theme.tokens.colors;
        let pad = 14.0;
        let inner_w = body.width() - 2.0 * pad;
        let header_y = body.y0 + 12.0;
        // Scrollable region starts below the fixed header line.
        let mut y = body.y0 + 12.0 + 22.0 - self.sidebar_scroll;

        let total = self.comments.len();
        let open = self.comments.iter().filter(|c| !c.resolved).count();
        tr.draw_text(
            scene,
            &format!("{} threads · {} open", total, open),
            11.0,
            theme.text_dim,
            base * Affine::translate((body.x0 + pad, header_y)),
            None,
        );

        for cm in &self.comments {
            let selected = self
                .selected_anno
                .as_deref()
                .map(|sa| cm.anno_id.as_deref() == Some(sa))
                .unwrap_or(false);
            // measure body height
            let (_, bh) = tr.measure(&cm.body, 12.0, Some(inner_w as f32 - 16.0));
            let mut card_h = 18.0 + 6.0 + bh as f64 + 12.0; // header + body
            let mut reply_heights = Vec::new();
            for r in &cm.replies {
                let (_, rh) = tr.measure(&r.body, 11.0, Some(inner_w as f32 - 36.0));
                reply_heights.push(rh as f64);
                card_h += 16.0 + rh as f64 + 8.0;
            }
            let card = Rect::new(body.x0 + pad, y, body.x1 - pad, y + card_h);

            // Cull cards scrolled out of the body, but keep advancing `y`.
            if card.y1 < body.y0 || card.y0 > body.y1 {
                hits.push((SidebarHit::Comment(cm.id.clone()), card));
                y += card_h + 12.0;
                continue;
            }

            // card bg
            let bg = if selected { render::alpha(theme.accent_primary, 28) } else { c.surfaces.base };
            scene.fill(Fill::NonZero, base, bg, None, &RoundedRect::from_rect(card, theme.tokens.radii.md as f64));
            let border = if selected { theme.accent_primary } else { c.border_standard };
            scene.stroke(&Stroke::new(if selected { 1.5 } else { 1.0 }), base, border, None, &RoundedRect::from_rect(card, theme.tokens.radii.md as f64));

            // author avatar + name + page pin
            let av_c = Point::new(card.x0 + 12.0 + 9.0, card.y0 + 14.0);
            draw_avatar(scene, base, tr, theme, &cm.author, av_c, 9.0);
            tr.draw_text_weighted(
                scene,
                &cm.author,
                11.5,
                theme.text_primary,
                base * Affine::translate((av_c.x + 14.0, card.y0 + 8.0)),
                None,
                TextWeight::Semibold,
            );
            // Right-edge chips: a delete (×) button, then the resolve chip.
            let del = Rect::new(card.x1 - 10.0 - 18.0, card.y0 + 6.0, card.x1 - 10.0, card.y0 + 24.0);
            let del_hot = del.contains(self.last_mouse);
            if del_hot {
                scene.fill(Fill::NonZero, base, render::alpha(c.semantic.danger, 40), None, &RoundedRect::from_rect(del, theme.tokens.radii.sm as f64));
            }
            let dc = Point::new((del.x0 + del.x1) / 2.0, (del.y0 + del.y1) / 2.0);
            let dcol = if del_hot { c.semantic.danger } else { theme.text_dim };
            scene.stroke(&Stroke::new(1.5), base, dcol, None, &Line::new((dc.x - 4.0, dc.y - 4.0), (dc.x + 4.0, dc.y + 4.0)));
            scene.stroke(&Stroke::new(1.5), base, dcol, None, &Line::new((dc.x - 4.0, dc.y + 4.0), (dc.x + 4.0, dc.y - 4.0)));
            hits.push((SidebarHit::DeleteComment(cm.id.clone()), del));

            let chip_w = 54.0;
            let chip = Rect::new(del.x0 - 8.0 - chip_w, card.y0 + 6.0, del.x0 - 8.0, card.y0 + 22.0);
            let (chip_label, chip_fill, chip_text) = if cm.resolved {
                ("Resolved", c.semantic.success, Color::WHITE)
            } else {
                ("Resolve", c.surface_white_10, theme.text_dim)
            };
            scene.fill(Fill::NonZero, base, chip_fill, None, &RoundedRect::from_rect(chip, theme.tokens.radii.full as f64));
            let (cw2, _) = tr.measure(chip_label, 9.5, None);
            tr.draw_text(
                scene,
                chip_label,
                9.5,
                chip_text,
                base * Affine::translate(((chip.x0 + chip.x1) / 2.0 - cw2 as f64 / 2.0, vcenter(chip, 9.5))),
                None,
            );
            hits.push((SidebarHit::Resolve(cm.id.clone()), chip));

            // body
            let mut yy = card.y0 + 24.0;
            tr.draw_text(
                scene,
                &cm.body,
                12.0,
                theme.text_primary,
                base * Affine::translate((card.x0 + 12.0, yy)),
                Some(inner_w as f32 - 16.0),
            );
            yy += bh as f64 + 10.0;

            // replies
            for (i, r) in cm.replies.iter().enumerate() {
                let rail = Rect::new(card.x0 + 14.0, yy - 2.0, card.x0 + 15.5, yy + reply_heights[i] + 10.0);
                scene.fill(Fill::NonZero, base, c.border_standard, None, &rail);
                tr.draw_text_weighted(
                    scene,
                    &r.author,
                    10.0,
                    theme.text_dim,
                    base * Affine::translate((card.x0 + 24.0, yy)),
                    None,
                    TextWeight::Semibold,
                );
                yy += 14.0;
                tr.draw_text(
                    scene,
                    &r.body,
                    11.0,
                    theme.tokens.colors.text_primary,
                    base * Affine::translate((card.x0 + 24.0, yy)),
                    Some(inner_w as f32 - 36.0),
                );
                yy += reply_heights[i] + 10.0;
            }

            hits.push((SidebarHit::Comment(cm.id.clone()), card));
            y += card_h + 12.0;
        }
    }

    fn draw_outline(&self, scene: &mut Scene, base: Affine, tr: &mut TextRenderer, theme: &Theme, body: Rect, hits: &mut Vec<(SidebarHit, Rect)>) {
        let pad = 12.0;
        let mut y = body.y0 + 10.0;
        for item in &self.doc.outline {
            let row = Rect::new(body.x0 + 6.0, y, body.x1 - 6.0, y + 30.0);
            let current = item.page == self.current_page;
            if current {
                scene.fill(Fill::NonZero, base, render::alpha(theme.accent_primary, 30), None, &RoundedRect::from_rect(row, theme.tokens.radii.md as f64));
                let bar = Rect::new(row.x0, row.y0 + 4.0, row.x0 + 2.5, row.y1 - 4.0);
                scene.fill(Fill::NonZero, base, theme.accent_primary, None, &bar);
            } else if row.contains(self.last_mouse) {
                scene.fill(Fill::NonZero, base, theme.tokens.colors.surface_white_5, None, &RoundedRect::from_rect(row, theme.tokens.radii.md as f64));
            }
            let indent = pad + item.level as f64 * 16.0;
            let weight = if item.level == 0 { TextWeight::Semibold } else { TextWeight::Regular };
            let col = if current { theme.text_primary } else if item.level == 0 { theme.text_primary } else { theme.text_dim };
            tr.draw_text_weighted(
                scene,
                &item.title,
                12.0,
                col,
                base * Affine::translate((row.x0 + indent, vcenter(row, 12.0))),
                Some((row.width() - indent - 28.0) as f32),
                weight,
            );
            let pnum = format!("{}", item.page);
            let (pw, _) = tr.measure(&pnum, 11.0, None);
            tr.draw_text(
                scene,
                &pnum,
                11.0,
                theme.text_dim,
                base * Affine::translate((row.x1 - 10.0 - pw as f64, vcenter(row, 11.0))),
                None,
            );
            hits.push((SidebarHit::Outline(item.page), row));
            y += 32.0;
            if y > body.y1 {
                break;
            }
        }
    }

    fn draw_signatures(&self, scene: &mut Scene, base: Affine, tr: &mut TextRenderer, theme: &Theme, body: Rect, hits: &mut Vec<(SidebarHit, Rect)>) {
        let c = &theme.tokens.colors;
        let pad = 14.0;
        let mut y = body.y0 + 12.0;

        // Signer identity card.
        let id_card = Rect::new(body.x0 + pad, y, body.x1 - pad, y + 58.0);
        scene.fill(Fill::NonZero, base, c.surfaces.base, None, &RoundedRect::from_rect(id_card, theme.tokens.radii.md as f64));
        scene.stroke(&Stroke::new(1.0), base, c.border_standard, None, &RoundedRect::from_rect(id_card, theme.tokens.radii.md as f64));
        tr.draw_text(scene, "SIGNING AS", 9.0, theme.text_dim, base * Affine::translate((id_card.x0 + 12.0, id_card.y0 + 9.0)), None);
        tr.draw_text_weighted(scene, &self.signer_name, 13.0, theme.text_primary, base * Affine::translate((id_card.x0 + 12.0, id_card.y0 + 22.0)), None, TextWeight::Semibold);
        // mini signature preview
        let prev = Rect::new(id_card.x1 - 96.0, id_card.y0 + 8.0, id_card.x1 - 12.0, id_card.y1 - 10.0);
        render::draw_signature_preview(scene, base, render::ink(), prev, 1);
        y += 70.0;

        // Add-field button.
        let add = Rect::new(body.x0 + pad, y, body.x1 - pad, y + 38.0);
        let arming = self.tool == Tool::Sign;
        let fill = if arming { theme.accent_primary } else { c.surface_white_10 };
        scene.fill(Fill::NonZero, base, fill, None, &RoundedRect::from_rect(add, theme.tokens.radii.md as f64));
        let label = if arming { "Click a page to place signature" } else { "+  Add signature field" };
        let (lw, _) = tr.measure(label, 12.0, None);
        tr.draw_text_weighted(
            scene,
            label,
            12.0,
            if arming { Color::WHITE } else { theme.text_primary },
            base * Affine::translate(((add.x0 + add.x1) / 2.0 - lw as f64 / 2.0, vcenter(add, 12.0))),
            None,
            TextWeight::Semibold,
        );
        hits.push((SidebarHit::AddSignature, add));
        y += 52.0;

        tr.draw_text(scene, &format!("PLACED  ·  {}", self.signatures.len()), 9.5, theme.text_dim, base * Affine::translate((body.x0 + pad, y)), None);
        y += 20.0;
        // The placed list scrolls; the signer card + add button stay fixed.
        y -= self.sidebar_scroll;

        for (i, s) in self.signatures.iter().enumerate() {
            let row = Rect::new(body.x0 + pad, y, body.x1 - pad, y + 46.0);
            if row.y1 < body.y0 || row.y0 > body.y1 {
                hits.push((SidebarHit::SigJump(i), row));
                y += 56.0;
                continue;
            }
            let selected = self.selected_sig.as_deref() == Some(s.id.as_str());
            let bg = if selected { render::alpha(theme.accent_primary, 28) } else { c.surfaces.base };
            scene.fill(Fill::NonZero, base, bg, None, &RoundedRect::from_rect(row, theme.tokens.radii.md as f64));
            let border = if selected { theme.accent_primary } else { c.border_standard };
            scene.stroke(&Stroke::new(if selected { 1.5 } else { 1.0 }), base, border, None, &RoundedRect::from_rect(row, theme.tokens.radii.md as f64));
            let sg = Rect::new(row.x0 + 8.0, row.y0 + 8.0, row.x0 + 64.0, row.y1 - 8.0);
            render::draw_signature_preview(scene, base, theme.text_primary, sg, s.style);
            tr.draw_text_weighted(scene, &s.signer, 12.0, theme.text_primary, base * Affine::translate((sg.x1 + 12.0, row.y0 + 9.0)), None, TextWeight::Semibold);
            tr.draw_text(scene, &format!("Page {} · {}", s.page, s.date), 10.0, theme.text_dim, base * Affine::translate((sg.x1 + 12.0, row.y0 + 26.0)), None);

            // Delete (×) button on the right.
            let del = Rect::new(row.x1 - 10.0 - 18.0, row.y0 + 8.0, row.x1 - 10.0, row.y0 + 26.0);
            let del_hot = del.contains(self.last_mouse);
            if del_hot {
                scene.fill(Fill::NonZero, base, render::alpha(c.semantic.danger, 40), None, &RoundedRect::from_rect(del, theme.tokens.radii.sm as f64));
            }
            let dc = Point::new((del.x0 + del.x1) / 2.0, (del.y0 + del.y1) / 2.0);
            let dcol = if del_hot { c.semantic.danger } else { theme.text_dim };
            scene.stroke(&Stroke::new(1.5), base, dcol, None, &Line::new((dc.x - 4.0, dc.y - 4.0), (dc.x + 4.0, dc.y + 4.0)));
            scene.stroke(&Stroke::new(1.5), base, dcol, None, &Line::new((dc.x - 4.0, dc.y + 4.0), (dc.x + 4.0, dc.y - 4.0)));
            hits.push((SidebarHit::DeleteSignature(s.id.clone()), del));

            hits.push((SidebarHit::SigJump(i), row));
            y += 56.0;
        }
    }

    // ---- status bar -------------------------------------------------

    fn draw_status(&self, scene: &mut Scene, base: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let s = self.status_rect;
        let c = &theme.tokens.colors;
        scene.fill(Fill::NonZero, base, c.surfaces.raised, None, &s);
        let hair = Rect::new(s.x0, s.y0, s.x1, s.y0 + 1.0);
        scene.fill(Fill::NonZero, base, c.border_standard, None, &hair);

        let pad = 14.0;
        let size = 11.5f32;
        let ty = vcenter(s, size);

        // Left: active tool + hint.
        let left = format!("{}  ·  {} key", self.tool.label(), self.tool.hotkey());
        tr.draw_text(scene, &left, size, theme.text_dim, base * Affine::translate((s.x0 + pad, ty)), None);

        // Center: page nav.
        let (prev, label, next) = self.status_layout();
        draw_chevron(scene, base, prev, theme.text_dim, true);
        draw_chevron(scene, base, next, theme.text_dim, false);
        let lbl = format!("Page {} of {}", self.current_page, self.doc.page_count());
        let (lw, _) = tr.measure(&lbl, size, None);
        tr.draw_text(
            scene,
            &lbl,
            size,
            theme.text_primary,
            base * Affine::translate(((label.x0 + label.x1) / 2.0 - lw as f64 / 2.0, ty)),
            None,
        );

        // Right: counts.
        let right = format!(
            "{} annotations · {} comments · {} signatures",
            self.annotations.len(),
            self.comments.len(),
            self.signatures.len()
        );
        let (rw, _) = tr.measure(&right, size, None);
        tr.draw_text(scene, &right, size, theme.text_dim, base * Affine::translate((s.x1 - pad - rw as f64, ty)), None);
    }
}

// ----------------------------------------------------------------------
// Free-standing glyph helpers
// ----------------------------------------------------------------------

fn icon_inset(r: Rect) -> Rect {
    r.inflate(-10.0, -10.0)
}

fn swatch_label(s: Swatch) -> &'static str {
    match s {
        Swatch::Yellow => "Yellow",
        Swatch::Green => "Green",
        Swatch::Pink => "Pink",
        Swatch::Blue => "Blue",
        Swatch::Purple => "Purple",
    }
}

fn draw_tool_icon(scene: &mut Scene, base: Affine, tool: Tool, btn: Rect, color: Color) {
    let r = icon_inset(btn);
    match tool {
        Tool::Select => {
            let mut p = BezPath::new();
            p.move_to((r.x0 + 1.0, r.y0));
            p.line_to((r.x0 + 1.0, r.y1));
            p.line_to((r.x0 + r.width() * 0.34, r.y0 + r.height() * 0.66));
            p.line_to((r.x0 + r.width() * 0.58, r.y1 + 1.0));
            p.line_to((r.x0 + r.width() * 0.72, r.y1 - r.height() * 0.05));
            p.line_to((r.x0 + r.width() * 0.50, r.y0 + r.height() * 0.55));
            p.line_to((r.x1, r.y0 + r.height() * 0.5));
            p.close_path();
            scene.fill(Fill::NonZero, base, color, None, &p);
        }
        Tool::Highlight => {
            // marker nib + chisel stroke
            let mut tip = BezPath::new();
            tip.move_to((r.x0, r.y1));
            tip.line_to((r.x0 + r.width() * 0.28, r.y1 - r.height() * 0.22));
            tip.line_to((r.x1, r.y0 + r.height() * 0.18));
            tip.line_to((r.x1 - r.width() * 0.22, r.y0));
            tip.line_to((r.x0 + r.height() * 0.0, r.y1 - r.height() * 0.0));
            tip.close_path();
            scene.stroke(&Stroke::new(2.4), base, color, None, &tip);
            let under = Line::new((r.x0, r.y1 + 1.5), (r.x0 + r.width() * 0.42, r.y1 + 1.5));
            scene.stroke(&Stroke::new(2.0), base, color, None, &under);
        }
        Tool::Ink => {
            // pen body + squiggle
            let body = Line::new((r.x1, r.y0), (r.x0 + r.width() * 0.34, r.y1 - r.height() * 0.18));
            scene.stroke(&Stroke::new(2.2), base, color, None, &body);
            let mut nib = BezPath::new();
            nib.move_to((r.x0 + r.width() * 0.34, r.y1 - r.height() * 0.18));
            nib.line_to((r.x0, r.y1));
            nib.line_to((r.x0 + r.width() * 0.16, r.y1 - r.height() * 0.36));
            nib.close_path();
            scene.fill(Fill::NonZero, base, color, None, &nib);
        }
        Tool::Note => {
            let sq = Rect::new(r.x0 + 1.0, r.y0 + 1.0, r.x1 - 1.0, r.y1 - 1.0);
            scene.stroke(&Stroke::new(1.8), base, color, None, &RoundedRect::from_rect(sq, 2.0));
            for i in 0..3 {
                let ly = sq.y0 + 4.0 + i as f64 * 4.0;
                scene.stroke(&Stroke::new(1.4), base, color, None, &Line::new((sq.x0 + 3.0, ly), (sq.x1 - 3.0, ly)));
            }
        }
        Tool::Sign => {
            let mut p = BezPath::new();
            p.move_to((r.x0, r.y0 + r.height() * 0.55));
            p.curve_to(
                (r.x0 + r.width() * 0.25, r.y0 - r.height() * 0.1),
                (r.x0 + r.width() * 0.4, r.y1),
                (r.x0 + r.width() * 0.62, r.y0 + r.height() * 0.5),
            );
            p.curve_to(
                (r.x0 + r.width() * 0.8, r.y0),
                (r.x1, r.y1 * 0.0 + r.y0 + r.height() * 0.9),
                (r.x1, r.y0 + r.height() * 0.45),
            );
            scene.stroke(&Stroke::new(2.0), base, color, None, &p);
            scene.stroke(&Stroke::new(1.4), base, color, None, &Line::new((r.x0, r.y1 + 1.0), (r.x1, r.y1 + 1.0)));
        }
    }
}

fn draw_plus(scene: &mut Scene, base: Affine, btn: Rect, color: Color) {
    let c = Point::new((btn.x0 + btn.x1) / 2.0, (btn.y0 + btn.y1) / 2.0);
    let l = 6.0;
    scene.stroke(&Stroke::new(2.0), base, color, None, &Line::new((c.x - l, c.y), (c.x + l, c.y)));
    scene.stroke(&Stroke::new(2.0), base, color, None, &Line::new((c.x, c.y - l), (c.x, c.y + l)));
}

fn draw_minus(scene: &mut Scene, base: Affine, btn: Rect, color: Color) {
    let c = Point::new((btn.x0 + btn.x1) / 2.0, (btn.y0 + btn.y1) / 2.0);
    let l = 6.0;
    scene.stroke(&Stroke::new(2.0), base, color, None, &Line::new((c.x - l, c.y), (c.x + l, c.y)));
}

fn draw_search_glyph(scene: &mut Scene, base: Affine, btn: Rect, color: Color) {
    let c = Point::new((btn.x0 + btn.x1) / 2.0 - 1.5, (btn.y0 + btn.y1) / 2.0 - 1.5);
    scene.stroke(&Stroke::new(1.8), base, color, None, &Circle::new(c, 5.0));
    scene.stroke(&Stroke::new(1.8), base, color, None, &Line::new((c.x + 3.6, c.y + 3.6), (c.x + 7.0, c.y + 7.0)));
}

fn draw_panel_glyph(scene: &mut Scene, base: Affine, btn: Rect, color: Color) {
    let r = icon_inset(btn);
    scene.stroke(&Stroke::new(1.6), base, color, None, &RoundedRect::from_rect(r, 2.0));
    let divx = r.x0 + r.width() * 0.62;
    scene.stroke(&Stroke::new(1.6), base, color, None, &Line::new((divx, r.y0), (divx, r.y1)));
}

/// Left-rail toggle glyph: a panel with the divider near the *left* edge,
/// mirroring the right-anchored sidebar glyph so the two read distinctly.
fn draw_rail_glyph(scene: &mut Scene, base: Affine, btn: Rect, color: Color) {
    let r = icon_inset(btn);
    scene.stroke(&Stroke::new(1.6), base, color, None, &RoundedRect::from_rect(r, 2.0));
    let divx = r.x0 + r.width() * 0.38;
    scene.stroke(&Stroke::new(1.6), base, color, None, &Line::new((divx, r.y0), (divx, r.y1)));
}

fn draw_chevron(scene: &mut Scene, base: Affine, btn: Rect, color: Color, left: bool) {
    let c = Point::new((btn.x0 + btn.x1) / 2.0, (btn.y0 + btn.y1) / 2.0);
    let w = 4.0;
    let h = 6.0;
    let mut p = BezPath::new();
    if left {
        p.move_to((c.x + w, c.y - h));
        p.line_to((c.x - w, c.y));
        p.line_to((c.x + w, c.y + h));
    } else {
        p.move_to((c.x - w, c.y - h));
        p.line_to((c.x + w, c.y));
        p.line_to((c.x - w, c.y + h));
    }
    scene.stroke(&Stroke::new(2.0), base, color, None, &p);
}

fn draw_avatar(scene: &mut Scene, base: Affine, tr: &mut TextRenderer, theme: &Theme, name: &str, center: Point, radius: f64) {
    // deterministic colour from the name
    let mut h: u32 = 2166136261;
    for byte in name.bytes() {
        h = (h ^ byte as u32).wrapping_mul(16777619);
    }
    let ramp = &theme.tokens.colors.accent;
    let col = ramp[(3 + (h as usize % 4)) % ramp.len()];
    scene.fill(Fill::NonZero, base, col, None, &Circle::new(center, radius));
    let initial = name.chars().next().unwrap_or('?').to_uppercase().to_string();
    let (iw, _) = tr.measure(&initial, 10.0, None);
    tr.draw_text_weighted(
        scene,
        &initial,
        10.0,
        Color::WHITE,
        base * Affine::translate((center.x - iw as f64 / 2.0, center.y - 6.5)),
        None,
        TextWeight::Bold,
    );
}
