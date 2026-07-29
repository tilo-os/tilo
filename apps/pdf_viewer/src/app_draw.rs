// Included into `app.rs` (shares its module + imports). Holds the chrome
// drawing routines and the pure layout helpers that both `draw` and event
// hit-testing rely on, so a click always lands where the pixel is.

/// Measured heights for a single comment card (shared by scroll layout + draw).
struct CommentMetrics {
    card_h: f64,
    body_h: f64,
    reply_heights: Vec<f64>,
    /// Height contributed by the reply-compose row, including its top gap.
    compose_h: f64,
}

/// Non-overlapping layout of a comment/note card's header row. The author name
/// gets a bounded width so a long name (or an email address) truncates instead
/// of running under the Resolve chip and the × button. Pure geometry so both
/// the draw pass and a test can assert the pieces never collide.
struct CommentHeader {
    avatar_c: Point,
    name_x: f64,
    /// Baseline (top) y for the author name.
    name_y: f64,
    /// Max text width for the author name before the resolve chip.
    name_max: f32,
    chip: Rect,
    del: Rect,
    /// Top y where the card body should begin, clear of the header row.
    body_top: f64,
}

struct ToolbarLayout {
    open: Rect,
    save: Rect,
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

/// Truncate `text` with an ellipsis so it fits within `max` px on one line.
fn truncate_to_width(tr: &mut TextRenderer, text: &str, size: f32, max: f32) -> String {
    if tr.measure(text, size, None).0 <= max {
        return text.to_string();
    }
    let mut kept = String::new();
    for ch in text.chars() {
        let mut probe = kept.clone();
        probe.push(ch);
        probe.push('…');
        if tr.measure(&probe, size, None).0 > max {
            break;
        }
        kept.push(ch);
    }
    kept.push('…');
    kept
}

impl PdfApp {
    // ---- pure layout ------------------------------------------------

    /// Whether the toolbar is wide enough to show the "PDF" wordmark + title.
    fn tb_show_wordmark(&self) -> bool {
        self.toolbar_rect.width() >= 980.0
    }

    fn toolbar_layout(&self) -> ToolbarLayout {
        let tb = self.toolbar_rect;
        let cy = (tb.y0 + tb.y1) / 2.0;
        let w = tb.width();
        let compact = w < 640.0;
        let bw = 34.0;
        let bh = 34.0;
        let gap = 6.0;
        let row = |x0: f64, width: f64| Rect::new(x0, cy - bh / 2.0, x0 + width, cy + bh / 2.0);

        // ---- right cluster, anchored to the right edge ----
        // Rightmost first, walking left; hidden pieces collapse to ZERO.
        let mut rx = tb.x1 - 16.0;
        let place = |width: f64, rx: &mut f64| {
            *rx -= width;
            row(*rx, width)
        };
        let toggle_sidebar = place(bw, &mut rx);
        rx -= gap;
        let search = place(bw, &mut rx);
        rx -= 14.0;
        let fitw = if compact { bw } else { 40.0 };
        let fit = place(fitw, &mut rx);
        rx -= 10.0;
        let zoom_in = place(28.0, &mut rx);
        let zoom_label = if compact { Rect::ZERO } else { place(46.0, &mut rx) };
        let zoom_out = place(28.0, &mut rx);
        let right_start = zoom_out.x0;

        // ---- left cluster: page-rail toggle (LEFT-MOST) + wordmark + Open + Save ----
        let toggle_rail = row(tb.x0 + 12.0, bw);
        let wordmark_x = toggle_rail.x1 + 10.0;
        let left_w = if self.tb_show_wordmark() { 150.0 } else { 20.0 };
        let open = row(wordmark_x + left_w + 8.0, bw);
        let save = row(open.x1 + gap, bw);
        let left_end = save.x1;

        // ---- centre: tools, centred in the remaining span ----
        let n = Tool::ALL.len() as f64;
        let tools_w = n * bw + (n - 1.0) * gap;
        let centre = (left_end + right_start) / 2.0;
        let mut tx = (centre - tools_w / 2.0).max(left_end + 8.0);
        let mut tools = Vec::new();
        for t in Tool::ALL {
            tools.push((t, row(tx, bw)));
            tx += bw + gap;
        }
        let tools_end = tx - gap;

        // Swatches sit just right of the tools, but only when the annotate
        // tools are active and there is genuinely room for them.
        let annotate = matches!(self.tool, Tool::Highlight | Tool::Ink | Tool::Note);
        let sw = 16.0;
        let sgap = 8.0;
        let swatch_span = 5.0 * sw + 4.0 * sgap;
        let sx0 = tools_end + 14.0;
        let mut swatches = Vec::new();
        if annotate && sx0 + swatch_span <= right_start - 10.0 {
            let mut sx = sx0;
            for s in Swatch::ALL {
                swatches.push((s, Rect::new(sx, cy - sw / 2.0, sx + sw, cy + sw / 2.0)));
                sx += sw + sgap;
            }
        }

        ToolbarLayout {
            open,
            save,
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
        out.push(("Open a PDF…  (Ctrl O)".to_string(), lay.open));
        out.push(("Save annotated PDF…  (Ctrl S)".to_string(), lay.save));
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
        let hovering = hovered.is_some();
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
        // Animate only while a hover is still counting toward the reveal delay;
        // a shown or absent tooltip needs no further frames.
        self.tooltip_pending = hovering && !self.tooltip.is_visible();
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

        // In text mode, refresh the page-point text-line cache for visible
        // pages so the smart highlight tool can snap to exactly the glyphs
        // being drawn. (Raster pages carry no toolkit text geometry.)
        if self.raster.is_none() {
            self.refresh_text_lines(tr, theme);
        } else {
            self.text_lines.clear();
            // Raster backend: refresh the real pdfium text layer for the
            // visible pages so selection + search overlays line up with glyphs.
            self.refresh_raster_text();
        }

        // Resolve which toolbar button (if any) the pointer is hovering and
        // advance the tooltip reveal timer before the toolbar paints.
        self.resolve_tooltip();

        // Resolve sidebar scroll + any reveal request before painting it.
        self.prepare_sidebar_scroll(tr);

        // Resolve the page rail's geometry for this frame before rasterising, so
        // the thumbnail slots (which pages, at what width) are known.
        if self.show_rail {
            self.page_sidebar.set_aspect(self.ph() / self.pw());
            let cur = self.current_page.saturating_sub(1);
            self.page_sidebar.begin_frame(self.rail_rect, self.doc.page_count(), cur);
            // Screenshot hook: stage a live reorder drag so a still frame shows
            // the floating ghost + the reflow gap. Never armed at runtime.
            if self.shots_rail_drag {
                let rects = self.page_sidebar.thumb_rects();
                if rects.len() >= 3 {
                    let from = rects[0].1.center();
                    let to = rects[2].1.center();
                    self.page_sidebar.on_press(from);
                    self.page_sidebar.on_motion(to);
                    let _ = self.page_sidebar.take_events();
                }
            }
        }

        // Rasterise the pages + thumbnails on screen once, up front.
        let (page_imgs, thumb_imgs) = self.rasterize_visible();

        self.draw_canvas(scene, base, tr, theme, &page_imgs);

        // The floating selection HUD over the page (shared `ui` component).
        self.draw_selection_hud(scene, base, tr, theme);

        // On a narrow window the panels float above the canvas behind a scrim.
        if self.overlay_mode && (self.show_rail || self.show_sidebar) {
            let strip = Rect::new(self.canvas_rect.x0, self.canvas_rect.y0, self.canvas_rect.x1, self.canvas_rect.y1);
            scene.fill(Fill::NonZero, base, Color::from_rgba8(0, 0, 0, 120), None, &strip);
        }

        self.draw_rail(scene, base, tr, theme, &thumb_imgs);
        let mut edit_rect = Rect::ZERO;
        let hits = self.draw_sidebar(scene, base, tr, theme, &mut edit_rect);
        self.edit_rect = edit_rect;
        self.draw_toolbar(scene, base, tr, theme);
        if self.search_active {
            let shits = self.draw_search(scene, base, tr, theme);
            self.search_hits = shits;
        } else {
            self.search_hits.clear();
        }
        self.draw_status(scene, base, tr, theme);
        // Context menu + tooltip composite above everything else. File
        // open/save is the host desktop's portal picker (HIG §5), so there is
        // no in-app dialog to draw here.
        self.draw_context_menu(scene, base, tr, theme);
        self.draw_tooltip(scene, base, tr, theme);
        // A transient toast (save confirmation, copy confirmation) on top.
        self.draw_toast(scene, base, tr, theme);

        self.sidebar_hits = hits;
    }


    /// Rasterise every page (and thumbnail) currently on screen exactly once,
    /// returning bitmaps keyed by 1-based page number. Cache hits inside the
    /// rasteriser make scrolling and panning allocation-free; only a zoom
    /// change or a page scrolling into view triggers real work. Empty in text
    /// mode.
    fn rasterize_visible(
        &mut self,
    ) -> (
        std::collections::HashMap<usize, PenikoImage>,
        std::collections::HashMap<usize, PenikoImage>,
    ) {
        let mut page_imgs = std::collections::HashMap::new();
        let mut thumb_imgs = std::collections::HashMap::new();
        if self.raster.is_none() {
            return (page_imgs, thumb_imgs);
        }

        let canvas = self.canvas_rect;
        let pageh = self.ph() * self.zoom();
        let card_w_px = (self.pw() * self.zoom()).max(1.0);
        // Render a little above display resolution so pages stay crisp when
        // the compositor scales the surface.
        let page_bucket = ((card_w_px * RASTER_SS).round() as u32).clamp(96, 4096);
        let mut want_pages = Vec::new();
        for (i, o) in self.page_origins().iter().enumerate() {
            if o.y > canvas.y1 || o.y + pageh < canvas.y0 {
                continue;
            }
            want_pages.push(i);
        }

        let mut want_thumbs: Vec<(usize, u32)> = Vec::new();
        if self.show_rail {
            let rail = self.rail_rect;
            // 0-based display indices + widths, from the shared rail's geometry.
            for (i, r) in self.page_sidebar.thumb_rects() {
                if r.y0 > rail.y1 || r.y1 < rail.y0 {
                    continue;
                }
                let tw = ((r.width() * 2.0).round() as u32).clamp(96, 600);
                want_thumbs.push((i, tw));
            }
        }

        // Full-page renders are the expensive part of a frame (a real page can
        // take tens to hundreds of ms). Rasterise only a small budget of *new*
        // (uncached) pages per frame so the open→first-paint path pays for the
        // single top page and lets the rest — a page's bottom sliver peeking in,
        // pages scrolling into view — fill in over the following frames. Cached
        // pages always draw (a cache hit is ~free).
        let mut new_budget = MAX_NEW_PAGE_RASTERS_PER_FRAME;
        let mut skipped_renderable = false;
        let mut rendered_new = false;
        if let Some(rz) = self.raster.as_mut() {
            for i in want_pages {
                if !rz.is_page_cached(i, page_bucket) {
                    if new_budget == 0 {
                        skipped_renderable = true;
                        continue;
                    }
                    new_budget -= 1;
                    if rz.render(i, page_bucket).map(|img| page_imgs.insert(i + 1, img)).is_some() {
                        rendered_new = true;
                    }
                } else if let Some(img) = rz.render(i, page_bucket) {
                    page_imgs.insert(i + 1, img);
                }
            }
            // Rail thumbnails are also real page rasters — for a scanned PDF a
            // single 192px thumbnail can cost 15-30ms, so a rail-full on the
            // open frame added ~90ms. Budget them the same way: a couple of new
            // ones per frame, the rest deferred to follow-up frames.
            let mut thumb_budget = MAX_NEW_THUMB_RASTERS_PER_FRAME;
            for (i, tw) in want_thumbs {
                if !rz.is_page_cached(i, tw) {
                    if thumb_budget == 0 {
                        skipped_renderable = true;
                        continue;
                    }
                    thumb_budget -= 1;
                    if rz.render(i, tw).map(|img| thumb_imgs.insert(i + 1, img)).is_some() {
                        rendered_new = true;
                    }
                } else if let Some(img) = rz.render(i, tw) {
                    thumb_imgs.insert(i + 1, img);
                }
            }
            rz.prune(page_bucket);
        }
        // Schedule a follow-up frame to render the pages we skipped this one —
        // but only when *this* frame made forward progress (rendered a new page).
        // A page that persistently fails to render therefore can't wedge the
        // frame pump into a permanent ~60 Hz spin.
        self.pending_raster = skipped_renderable && rendered_new;
        (page_imgs, thumb_imgs)
    }

    /// Populate `self.raster_text` with the pdfium text layer of the pages
    /// currently in view (raster backend only). pdfium's own text-page cache
    /// makes repeated frames cheap; only a page scrolling into view does work.
    fn refresh_raster_text(&mut self) {
        if self.raster.is_none() {
            return;
        }
        // The pdfium text layer only feeds the selection + search-hit overlays
        // and is expensive to extract (hundreds of ms per page). On an idle or
        // scroll frame — no selection, no search hits — skip it entirely so
        // opening a document and scrolling stay smooth. A selection that starts
        // on a page whose layer isn't built yet extracts it on demand inside
        // `begin_selection`; a search extracts every page up front in
        // `recompute_matches`, so the visible ones are cache hits here.
        let need = self.selection.is_some() || !self.raster_matches.is_empty();
        if !need {
            self.raster_text.clear();
            return;
        }
        let pageh = self.ph() * self.zoom();
        let canvas = self.canvas_rect;
        let origins = self.page_origins();
        self.raster_text.clear();
        let mut want = Vec::new();
        for (i, o) in origins.iter().enumerate() {
            if o.y > canvas.y1 || o.y + pageh < canvas.y0 {
                continue;
            }
            want.push(i);
        }
        for i in want {
            if let Some(rz) = self.raster.as_mut() {
                let pt = rz.page_text(i);
                self.raster_text.insert(i + 1, pt);
            }
        }
    }

    /// Populate `self.text_lines` for the pages currently in view. Cheap: the
    /// measurer caches per (text,size,width) so repeated frames hit the cache.
    fn refresh_text_lines(&mut self, tr: &mut TextRenderer, theme: &Theme) {
        let pageh = self.ph() * self.zoom();
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

    /// The no-document surface: a document glyph, a short line of guidance,
    /// and — if the last open failed — the reason.
    fn draw_empty_state(&self, scene: &mut Scene, base: Affine, tr: &mut TextRenderer, theme: &Theme, canvas: Rect) {
        let c = &theme.tokens.colors;
        let cx = (canvas.x0 + canvas.x1) / 2.0;
        let cy = (canvas.y0 + canvas.y1) / 2.0;

        // Outlined page glyph with a folded corner.
        let gw = 78.0;
        let gh = 98.0;
        let g = Rect::new(cx - gw / 2.0, cy - gh / 2.0 - 30.0, cx + gw / 2.0, cy + gh / 2.0 - 30.0);
        scene.fill(Fill::NonZero, base, c.surfaces.raised, None, &RoundedRect::from_rect(g, 8.0));
        scene.stroke(&Stroke::new(1.5), base, c.border_standard, None, &RoundedRect::from_rect(g, 8.0));
        let fold = {
            let mut p = BezPath::new();
            p.move_to((g.x1 - 22.0, g.y0));
            p.line_to((g.x1, g.y0 + 22.0));
            p.line_to((g.x1 - 22.0, g.y0 + 22.0));
            p.close_path();
            p
        };
        scene.fill(Fill::NonZero, base, c.surface_white_10, None, &fold);
        for i in 0..3 {
            let ly = g.y0 + 44.0 + i as f64 * 12.0;
            scene.stroke(&Stroke::new(2.0), base, c.border_standard, None, &Line::new((g.x0 + 16.0, ly), (g.x1 - 16.0, ly)));
        }

        let (title, sub) = match &self.load_error {
            Some(err) => ("Couldn't open this document", err.as_str()),
            None => ("No document open", "Choose a PDF to start reading."),
        };
        let (tw, _) = tr.measure(title, 16.0, None);
        tr.draw_text_weighted(
            scene,
            title,
            16.0,
            theme.text_primary,
            base * Affine::translate((cx - tw as f64 / 2.0, cy + 34.0)),
            None,
            TextWeight::Semibold,
        );
        let (sw, _) = tr.measure(sub, 12.5, Some((canvas.width() - 80.0) as f32));
        tr.draw_text(
            scene,
            sub,
            12.5,
            theme.text_dim,
            base * Affine::translate((cx - sw as f64 / 2.0, cy + 58.0)),
            Some((canvas.width() - 80.0) as f32),
        );

        // Primary action: open a document from within the app.
        let btn = self.empty_open_button_rect();
        let hot = btn.contains(self.last_mouse);
        let shape = RoundedRect::from_rect(btn, theme.tokens.radii.md as f64);
        scene.fill(Fill::NonZero, base, theme.accent_primary, None, &shape);
        if hot {
            scene.fill(Fill::NonZero, base, c.surface_white_10, None, &shape);
        }
        // Folder glyph + label as a centred group.
        let label = "Open a PDF…";
        let (lw, _) = tr.measure(label, 13.0, None);
        let gsz = 16.0;
        let gap = 8.0;
        let group_w = gsz + gap + lw as f64;
        let gx = btn.x0 + (btn.width() - group_w) / 2.0;
        let gcy = (btn.y0 + btn.y1) / 2.0;
        draw_folder_glyph(scene, base, Rect::new(gx, gcy - gsz / 2.0, gx + gsz, gcy + gsz / 2.0), Color::WHITE);
        tr.draw_text_weighted(
            scene,
            label,
            13.0,
            Color::WHITE,
            base * Affine::translate((gx + gsz + gap, vcenter(btn, 13.0))),
            None,
            TextWeight::Semibold,
        );
    }

    fn draw_canvas(
        &self,
        scene: &mut Scene,
        base: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        page_imgs: &std::collections::HashMap<usize, PenikoImage>,
    ) {
        let canvas = self.canvas_rect;
        scene.fill(Fill::NonZero, base, theme.tokens.colors.surfaces.base, None, &canvas);

        // No document open: a calm empty surface, never fabricated content.
        if !self.has_document {
            self.draw_empty_state(scene, base, tr, theme, canvas);
            return;
        }

        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, base, &canvas);

        let pageh = self.ph() * self.zoom();
        let origins = self.page_origins();
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
                self.zoom(),
                self.pw(),
                self.ph(),
                page_imgs.get(&page_num),
                &matched,
                active,
                &self.annotations,
                &self.signatures,
                self.selected_anno.as_deref(),
            );

            // Raster backend: draw search-hit + selection bands over the page
            // image, using the real glyph geometry so they land on the text.
            if self.raster.is_some() {
                self.draw_text_overlays(scene, base, theme, page_num, *origin);
            }
        }

        // Drag preview.
        if let Some(d) = &self.drag {
            if let Some(origin) = origins.get(d.page.saturating_sub(1)) {
                let col = render::swatch_color(theme, self.swatch);
                match d.tool {
                    Tool::Highlight => {
                        let r = PtRect::from_corners(d.start.0, d.start.1, d.cur.0, d.cur.1);
                        let scr = Rect::new(
                            origin.x + r.x * self.zoom(),
                            origin.y + r.y * self.zoom(),
                            origin.x + (r.x + r.w) * self.zoom(),
                            origin.y + (r.y + r.h) * self.zoom(),
                        );
                        scene.fill(Fill::NonZero, base, render::alpha(col, 80), None, &scr);
                        scene.stroke(&Stroke::new(1.0), base, col, None, &scr);
                    }
                    Tool::Ink => {
                        if d.points.len() >= 2 {
                            let mut path = BezPath::new();
                            let p0 = Point::new(origin.x + d.points[0].0 * self.zoom(), origin.y + d.points[0].1 * self.zoom());
                            path.move_to(p0);
                            for p in &d.points[1..] {
                                path.line_to((origin.x + p.0 * self.zoom(), origin.y + p.1 * self.zoom()));
                            }
                            scene.stroke(&Stroke::new((2.4 * self.zoom()).max(1.5)), base, col, None, &path);
                        }
                    }
                    _ => {}
                }
            }
        }

        scene.pop_layer();

        // Vertical + horizontal scrollbars: painted by the shared viewport
        // kernel so they match the other document apps (they appear only when
        // the content overflows the viewport on that axis).
        self.viewport.draw_scrollbars(scene, base, theme);
    }

    /// Draw the raster text-layer overlays for one page: every search hit as a
    /// translucent band (the active hit emphasised) and the live selection as
    /// an accent band. All rectangles come from the real pdfium glyph boxes, so
    /// they sit exactly over the printed characters.
    fn draw_text_overlays(&self, scene: &mut Scene, base: Affine, theme: &Theme, page_num: usize, origin: Point) {
        let z = self.zoom();
        let to_screen = |r: &PtRect| {
            Rect::new(
                origin.x + r.x * z,
                origin.y + r.y * z,
                origin.x + (r.x + r.w) * z,
                origin.y + (r.y + r.h) * z,
            )
        };

        // Image-region marquee (Shift + Select): a translucent accent rectangle
        // with a dashed border, drawn even when the page has no text layer.
        if let Some(region) = self.region {
            if region.page == page_num {
                let sr = to_screen(&region.rect());
                scene.fill(Fill::NonZero, base, render::alpha(theme.accent_primary, 40), None, &sr);
                scene.stroke(
                    &Stroke::new(1.5).with_dashes(0.0, [5.0, 4.0]),
                    base,
                    theme.accent_primary,
                    None,
                    &sr,
                );
            }
        }

        let Some(layer) = self.raster_text.get(&page_num) else { return };

        // Search hits.
        let hit_col = theme.tokens.colors.semantic.warning;
        for (i, m) in self.raster_matches.iter().enumerate() {
            if m.page != page_num {
                continue;
            }
            let active = i == self.active_match;
            for band in layer.bands_for_range(m.char_start, m.char_start + m.char_len) {
                let sr = to_screen(&band);
                let fill = if active { render::alpha(hit_col, 150) } else { render::alpha(hit_col, 70) };
                scene.fill(Fill::NonZero, base, fill, None, &RoundedRect::from_rect(sr, 1.5));
                if active {
                    scene.stroke(&Stroke::new(1.25), base, hit_col, None, &RoundedRect::from_rect(sr, 1.5));
                }
            }
        }

        // Live selection.
        if let Some(sel) = self.selection {
            if sel.page == page_num && sel.is_active() {
                let (a, b) = sel.range();
                for band in layer.bands_for_range(a, b) {
                    let sr = to_screen(&band);
                    scene.fill(Fill::NonZero, base, render::alpha(theme.accent_primary, 70), None, &sr);
                }
            }
        }
    }

    /// The floating selection HUD — the shared `ui::SelectionHud`, identical to
    /// the ebook reader's bar: highlight swatches + Copy + Note over the live
    /// text selection. Rebuilt each frame and stored for the next press to
    /// hit-test. Only shown once the drag has settled (a live drag would make
    /// the bar chase the pointer) and never while the inline editor is open.
    fn draw_selection_hud(&mut self, scene: &mut Scene, base: Affine, tr: &mut TextRenderer, theme: &Theme) {
        self.sel_hud = None;
        let Some(sel) = self.selection else { return };
        if sel.dragging || !sel.is_active() || self.editing.is_some() {
            return;
        }
        let Some(bounds_rect) = self.selection_screen_bounds() else { return };
        // Anchor above the top edge of the selection, horizontally centred on it.
        let anchor = Point::new((bounds_rect.x0 + bounds_rect.x1) / 2.0, bounds_rect.y0);
        let c = &theme.tokens.colors;
        let style = ui::ReaderHudStyle {
            surface: c.surfaces.raised,
            border: c.border_standard,
            shadow: Color::from_rgba8(0, 0, 0, 70),
            text: c.text_primary,
            hover: theme.bg_hover,
            pill: c.surfaces.overlay,
            ring: theme.accent_primary,
            danger: c.semantic.danger,
        };
        let swatches: Vec<ui::HudSwatch> = Swatch::ALL
            .iter()
            .map(|&s| {
                let (r, g, b) = s.rgb();
                ui::HudSwatch::new([r, g, b], s == self.swatch)
            })
            .collect();
        let hud = ui::SelectionHud::draw(
            scene,
            base,
            tr,
            &style,
            &swatches,
            true,
            false,
            self.last_mouse,
            anchor,
            self.canvas_rect,
        );
        self.sel_hud = Some(hud);
    }

    /// A transient toast pill (e.g. "Saved to …" or "Copied 42 characters")
    /// centred near the bottom of the canvas. Set on an action, cleared by the
    /// next document interaction (Esc, a new open/load).
    fn draw_toast(&self, scene: &mut Scene, base: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let Some(note) = self.status_note.as_deref() else { return };
        if note.is_empty() {
            return;
        }
        let c = &theme.tokens.colors;
        let size = theme.tokens.typography.scale.sm;
        let (tw, _) = tr.measure(note, size, None);
        let pad_x = 14.0;
        let w = (tw as f64 + 2.0 * pad_x).min(self.canvas_rect.width() - 24.0);
        let h = 30.0;
        let cx = (self.canvas_rect.x0 + self.canvas_rect.x1) / 2.0;
        let y1 = self.canvas_rect.y1 - 18.0;
        let r = Rect::new(cx - w / 2.0, y1 - h, cx + w / 2.0, y1);
        let shape = RoundedRect::from_rect(r, theme.tokens.radii.full as f64);
        let sh = RoundedRect::new(r.x0, r.y0 + 3.0, r.x1, r.y1 + 3.0, theme.tokens.radii.full as f64);
        scene.fill(Fill::NonZero, base, Color::from_rgba8(0, 0, 0, 70), None, &sh);
        scene.fill(Fill::NonZero, base, c.surfaces.raised, None, &shape);
        scene.stroke(&Stroke::new(1.0), base, c.border_standard, None, &shape);
        tr.draw_text(
            scene,
            note,
            size,
            c.text_primary,
            base * Affine::translate((r.x0 + pad_x, vcenter(r, size))),
            Some((w - 2.0 * pad_x) as f32),
        );
    }

    // ---- thumbnail rail --------------------------------------------

    fn draw_rail(
        &mut self,
        scene: &mut Scene,
        base: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        thumb_imgs: &std::collections::HashMap<usize, PenikoImage>,
    ) {
        if !self.show_rail {
            return;
        }
        let rail = self.rail_rect;
        let markup = self.pages_with_markup();
        let pw = self.pw();

        // The shared page rail draws the panel, thumbnails, selection ring,
        // index numbers, drag reflow + ghost, scrollbar, context menu and the
        // "Import…" affordance. It renders the host's page content through this
        // closure — a real pdfium bitmap when the raster backend has one,
        // otherwise the abstract block sketch. Move the rail out of `self` so
        // the closure can borrow the (immutable) document alongside it.
        let mut sb = std::mem::replace(&mut self.page_sidebar, PageSidebar::new(1.0));
        let doc = &self.doc;
        sb.render(scene, base, tr, theme, |scene, _tr, base, i, rect| {
            let num = i + 1;
            if let Some(page) = doc.page(num) {
                render::draw_thumbnail(
                    scene,
                    theme,
                    base,
                    page,
                    rect,
                    pw,
                    thumb_imgs.get(&num),
                    false,
                    markup.contains(&num),
                );
            }
        });
        self.page_sidebar = sb;

        // The right divider doubles as a resize grip; brighten it while the
        // pointer is over the grip strip or actively dragging it.
        let grip_hot = self.rail_drag.is_some()
            || (self.last_mouse.x >= rail.x1 - RAIL_GRIP
                && self.last_mouse.x <= rail.x1 + RAIL_GRIP
                && self.last_mouse.y >= rail.y0
                && self.last_mouse.y <= rail.y1);
        if grip_hot {
            let div = Rect::new(rail.x1 - 2.0, rail.y0, rail.x1, rail.y1);
            scene.fill(Fill::NonZero, base, theme.accent_primary, None, &div);
        }
    }

    // ---- toolbar ----------------------------------------------------

    fn draw_toolbar(&self, scene: &mut Scene, base: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let tb = self.toolbar_rect;
        let c = &theme.tokens.colors;
        scene.fill(Fill::NonZero, base, c.surfaces.raised, None, &tb);
        let hair = Rect::new(tb.x0, tb.y1 - 1.0, tb.x1, tb.y1);
        scene.fill(Fill::NonZero, base, c.border_standard, None, &hair);

        // Wordmark: the document name + page count. (No decorative glyph — it
        // read as a redundant, meaningless icon next to the filename.) It starts
        // just right of the left-most page-rail toggle button.
        let gx = tb.x0 + 12.0 + 34.0 + 10.0;
        if self.tb_show_wordmark() {
            let max = 148.0;
            let name = if self.doc_name.is_empty() { "PDF".to_string() } else { self.doc_name.clone() };
            let name = truncate_to_width(tr, &name, 13.0, max);
            tr.draw_text_weighted(
                scene,
                &name,
                13.0,
                theme.text_primary,
                base * Affine::translate((gx, vcenter(tb, 13.0) - 6.0)),
                None,
                TextWeight::Semibold,
            );
            let subtitle = if self.has_document {
                format!("{} pages", self.doc.page_count())
            } else {
                "PDF reader".to_string()
            };
            let subtitle = truncate_to_width(tr, &subtitle, 10.0, max);
            tr.draw_text(
                scene,
                &subtitle,
                10.0,
                theme.text_dim,
                base * Affine::translate((gx, vcenter(tb, 10.0) + 7.0)),
                None,
            );
        }

        let lay = self.toolbar_layout();
        let annotate = matches!(self.tool, Tool::Highlight | Tool::Ink | Tool::Note);

        // Open a document (folder button), just right of the wordmark.
        self.icon_button_bg(scene, base, theme, lay.open, false, lay.open.contains(self.last_mouse));
        draw_folder_glyph(scene, base, icon_inset(lay.open), theme.text_primary);

        // Save annotated PDF (tray/down-arrow). Dimmed until a document with the
        // pdfium backend is open, since flattening needs pdfium.
        let can_save = self.has_document && self.raster.is_some();
        self.icon_button_bg(scene, base, theme, lay.save, false, can_save && lay.save.contains(self.last_mouse));
        let save_col = if can_save { theme.text_primary } else { theme.text_dim };
        draw_save_glyph(scene, base, icon_inset(lay.save), save_col);
        // Unsaved page edits (reorder / delete / duplicate / import) surface as
        // an accent dot on the Save button.
        if self.pages_dirty && can_save {
            let d = 3.5;
            let dot = Circle::new((lay.save.x1 - 6.0, lay.save.y0 + 6.0), d);
            scene.fill(Fill::NonZero, base, theme.accent_primary, None, &dot);
        }

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
        if lay.zoom_label.area() > 1.0 {
            let zlabel = format!("{}%", (self.zoom() * 100.0).round() as i64);
            let (zw, _) = tr.measure(&zlabel, 12.0, None);
            tr.draw_text(
                scene,
                &zlabel,
                12.0,
                theme.text_primary,
                base * Affine::translate(((lay.zoom_label.x0 + lay.zoom_label.x1) / 2.0 - zw as f64 / 2.0, vcenter(lay.zoom_label, 12.0))),
                None,
            );
        }

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

        // Match counter (spans whichever backend's hits are live).
        let counter = if self.match_count() == 0 {
            if self.query.is_empty() { String::new() } else { "No matches".to_string() }
        } else {
            format!("{} of {} matches", self.active_match + 1, self.match_count())
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

    fn draw_sidebar(&self, scene: &mut Scene, base: Affine, tr: &mut TextRenderer, theme: &Theme, edit_rect: &mut Rect) -> Vec<(SidebarHit, Rect)> {
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
            SidebarTab::Comments => self.draw_comments(scene, base, tr, theme, body, &mut hits, edit_rect),
            SidebarTab::Outline => self.draw_outline(scene, base, tr, theme, body, &mut hits),
            SidebarTab::Signatures => self.draw_signatures(scene, base, tr, theme, body, &mut hits, edit_rect),
        }
        scene.pop_layer();
        hits
    }

    /// The scrollable body rect of the sidebar (below the tab strip).
    fn sidebar_body(&self) -> Rect {
        let sb = self.sidebar_rect;
        Rect::new(sb.x0, sb.y0 + 38.0, sb.x1, sb.y1)
    }

    /// Measured heights for one comment card. Computed once and shared by the
    /// scroll layout and the draw pass so the two never disagree about how tall
    /// a card is (which would misplace every card below it).
    /// Lay out a comment/note card header so the avatar, author name, resolve
    /// chip and delete button never overlap regardless of name length. The name
    /// is clamped to the space left of the chip (with an 8px gap), and the body
    /// starts below the header band.
    fn comment_header_layout(&self, card: Rect) -> CommentHeader {
        let del = Rect::new(card.x1 - 10.0 - 18.0, card.y0 + 6.0, card.x1 - 10.0, card.y0 + 24.0);
        let chip_w = 54.0;
        let chip = Rect::new(del.x0 - 8.0 - chip_w, card.y0 + 6.0, del.x0 - 8.0, card.y0 + 22.0);
        let avatar_c = Point::new(card.x0 + 12.0 + 9.0, card.y0 + 15.0);
        let name_x = avatar_c.x + 14.0;
        // Stop 8px short of the chip so a long name truncates cleanly.
        let name_max = ((chip.x0 - 8.0) - name_x).max(24.0) as f32;
        CommentHeader {
            avatar_c,
            name_x,
            name_y: card.y0 + 9.0,
            name_max,
            chip,
            del,
            body_top: card.y0 + 30.0,
        }
    }

    fn comment_metrics(&self, tr: &mut TextRenderer, cm: &Comment, inner_w: f64) -> CommentMetrics {
        // The body shows the live edit buffer while this comment is focused.
        let editing_body = self.editing.as_ref().filter(|e| e.target == EditTarget::CommentBody(cm.id.clone()));
        let body_text = editing_body.map(|e| e.buffer.as_str()).unwrap_or(cm.body.as_str());
        let body_probe = if body_text.is_empty() { "M" } else { body_text };
        let (_, bh) = tr.measure(body_probe, 12.0, Some(inner_w as f32 - 16.0));
        // Header band (avatar + name + chips) is 24px, then a 6px gap, the body,
        // and a 12px bottom pad. Mirrors `comment_header_layout`'s `body_top`.
        let mut card_h = 24.0 + 6.0 + bh as f64 + 12.0;
        let mut reply_heights = Vec::with_capacity(cm.replies.len());
        for r in &cm.replies {
            let (_, rh) = tr.measure(&r.body, 11.0, Some(inner_w as f32 - 36.0));
            reply_heights.push(rh as f64);
            card_h += 16.0 + rh as f64 + 8.0;
        }
        // Reply-compose row: a slim "Reply" affordance, or the open field.
        let composing = self.editing.as_ref().filter(|e| e.target == EditTarget::Reply(cm.id.clone()));
        let compose_h = if let Some(e) = composing {
            let probe = if e.buffer.is_empty() { "M" } else { e.buffer.as_str() };
            let (_, fh) = tr.measure(probe, 11.5, Some(inner_w as f32 - 40.0));
            8.0 + (fh as f64).max(15.0) + 16.0
        } else {
            8.0 + 24.0
        };
        card_h += compose_h;
        CommentMetrics { card_h, body_h: bh as f64, reply_heights, compose_h }
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
                    let card_h = self.comment_metrics(tr, cm, inner_w).card_h;
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

    fn draw_comments(&self, scene: &mut Scene, base: Affine, tr: &mut TextRenderer, theme: &Theme, body: Rect, hits: &mut Vec<(SidebarHit, Rect)>, edit_rect: &mut Rect) {
        let c = &theme.tokens.colors;
        let pad = 14.0;
        let inner_w = body.width() - 2.0 * pad;
        let header_y = body.y0 + 12.0;
        // Scrollable region starts below the fixed header line.
        let mut y = body.y0 + 12.0 + 22.0 - self.sidebar_scroll;

        let total = self.comments.len();
        let open = self.comments.iter().filter(|c| !c.resolved).count();
        let header = if total == 0 {
            "No comments yet".to_string()
        } else {
            format!("{} threads · {} open", total, open)
        };
        tr.draw_text(
            scene,
            &header,
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
            let m = self.comment_metrics(tr, cm, inner_w);
            let card_h = m.card_h;
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

            // Header row: avatar + author name (bounded so it never runs under
            // the chips) + resolve chip + delete button, laid out so nothing
            // overlaps regardless of the name's length.
            let hdr = self.comment_header_layout(card);
            draw_avatar(scene, base, tr, theme, &cm.author, hdr.avatar_c, 9.0);
            let name = truncate_to_width(tr, &cm.author, 11.5, hdr.name_max);
            tr.draw_text_weighted(
                scene,
                &name,
                11.5,
                theme.text_primary,
                base * Affine::translate((hdr.name_x, hdr.name_y)),
                Some(hdr.name_max),
                TextWeight::Semibold,
            );
            // Right-edge chips: a delete (×) button, then the resolve chip.
            let del = hdr.del;
            let del_hot = del.contains(self.last_mouse);
            if del_hot {
                scene.fill(Fill::NonZero, base, render::alpha(c.semantic.danger, 40), None, &RoundedRect::from_rect(del, theme.tokens.radii.sm as f64));
            }
            let dc = Point::new((del.x0 + del.x1) / 2.0, (del.y0 + del.y1) / 2.0);
            let dcol = if del_hot { c.semantic.danger } else { theme.text_dim };
            scene.stroke(&Stroke::new(1.5), base, dcol, None, &Line::new((dc.x - 4.0, dc.y - 4.0), (dc.x + 4.0, dc.y + 4.0)));
            scene.stroke(&Stroke::new(1.5), base, dcol, None, &Line::new((dc.x - 4.0, dc.y + 4.0), (dc.x + 4.0, dc.y - 4.0)));
            hits.push((SidebarHit::DeleteComment(cm.id.clone()), del));

            let chip = hdr.chip;
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

            // ---- body (click to edit; caret while focused) ----
            let body_x = card.x0 + 12.0;
            let body_top = hdr.body_top;
            let body_wrap = inner_w as f32 - 16.0;
            let editing_body = self.is_editing(&EditTarget::CommentBody(cm.id.clone()));
            let shown: &str = self
                .editing
                .as_ref()
                .filter(|_| editing_body)
                .map(|e| e.buffer.as_str())
                .unwrap_or(cm.body.as_str());
            let body_hit = Rect::new(body_x - 4.0, body_top - 4.0, card.x1 - 10.0, body_top + m.body_h + 6.0);
            if editing_body {
                scene.stroke(&Stroke::new(1.5), base, theme.border_focus, None, &RoundedRect::from_rect(body_hit, theme.tokens.radii.sm as f64));
                *edit_rect = body_hit;
            }
            if shown.is_empty() {
                tr.draw_text(scene, "Write a comment…", 12.0, theme.text_dim, base * Affine::translate((body_x, body_top)), Some(body_wrap));
            } else {
                tr.draw_text(scene, shown, 12.0, theme.text_primary, base * Affine::translate((body_x, body_top)), Some(body_wrap));
            }
            if editing_body {
                let caret = self.editing.as_ref().map(|e| e.caret).unwrap_or(0);
                draw_caret(scene, base, tr, theme.accent_primary, shown, 12.0, body_wrap, Point::new(body_x, body_top), caret);
            }
            hits.push((SidebarHit::EditComment(cm.id.clone()), body_hit));

            let mut yy = body_top + m.body_h + 10.0;

            // replies
            for (i, r) in cm.replies.iter().enumerate() {
                let rail = Rect::new(card.x0 + 14.0, yy - 2.0, card.x0 + 15.5, yy + m.reply_heights[i] + 10.0);
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
                yy += m.reply_heights[i] + 10.0;
            }

            // ---- reply composer ----
            let composing = self.is_editing(&EditTarget::Reply(cm.id.clone()));
            let compose_top = yy + 8.0;
            if composing {
                let field_h = m.compose_h - 8.0;
                let field = Rect::new(card.x0 + 12.0, compose_top, card.x1 - 12.0, compose_top + field_h);
                scene.fill(Fill::NonZero, base, c.surfaces.raised, None, &RoundedRect::from_rect(field, theme.tokens.radii.sm as f64));
                scene.stroke(&Stroke::new(1.5), base, theme.border_focus, None, &RoundedRect::from_rect(field, theme.tokens.radii.sm as f64));
                *edit_rect = field;
                let rbuf = self.editing.as_ref().map(|e| e.buffer.as_str()).unwrap_or("");
                let rwrap = (field.width() - 20.0) as f32;
                let tx = field.x0 + 10.0;
                let ty = field.y0 + 8.0;
                if rbuf.is_empty() {
                    tr.draw_text(scene, "Reply…", 11.5, theme.text_dim, base * Affine::translate((tx, ty)), Some(rwrap));
                } else {
                    tr.draw_text(scene, rbuf, 11.5, theme.text_primary, base * Affine::translate((tx, ty)), Some(rwrap));
                }
                let caret = self.editing.as_ref().map(|e| e.caret).unwrap_or(0);
                draw_caret(scene, base, tr, theme.accent_primary, rbuf, 11.5, rwrap, Point::new(tx, ty), caret);
                hits.push((SidebarHit::ReplyField(cm.id.clone()), field));
            } else {
                let btn = Rect::new(card.x0 + 12.0, compose_top, card.x1 - 12.0, compose_top + 22.0);
                let hot = btn.contains(self.last_mouse);
                if hot {
                    scene.fill(Fill::NonZero, base, c.surface_white_5, None, &RoundedRect::from_rect(btn, theme.tokens.radii.sm as f64));
                }
                tr.draw_text(
                    scene,
                    "+ Reply",
                    11.0,
                    if hot { theme.text_primary } else { theme.text_dim },
                    base * Affine::translate((btn.x0 + 6.0, vcenter(btn, 11.0))),
                    None,
                );
                hits.push((SidebarHit::ReplyField(cm.id.clone()), btn));
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

    fn draw_signatures(&self, scene: &mut Scene, base: Affine, tr: &mut TextRenderer, theme: &Theme, body: Rect, hits: &mut Vec<(SidebarHit, Rect)>, edit_rect: &mut Rect) {
        let c = &theme.tokens.colors;
        let pad = 14.0;
        let mut y = body.y0 + 12.0;

        // Signer identity card — the name is click-to-edit so a real user can
        // sign under their own identity.
        let id_card = Rect::new(body.x0 + pad, y, body.x1 - pad, y + 58.0);
        scene.fill(Fill::NonZero, base, c.surfaces.base, None, &RoundedRect::from_rect(id_card, theme.tokens.radii.md as f64));
        scene.stroke(&Stroke::new(1.0), base, c.border_standard, None, &RoundedRect::from_rect(id_card, theme.tokens.radii.md as f64));
        tr.draw_text(scene, "SIGNING AS", 9.0, theme.text_dim, base * Affine::translate((id_card.x0 + 12.0, id_card.y0 + 9.0)), None);

        let editing_signer = self.is_editing(&EditTarget::Signer);
        let name_shown: &str = self
            .editing
            .as_ref()
            .filter(|_| editing_signer)
            .map(|e| e.buffer.as_str())
            .unwrap_or(self.signer_name.as_str());
        let name_x = id_card.x0 + 12.0;
        let name_y = id_card.y0 + 22.0;
        let name_hit = Rect::new(id_card.x0 + 8.0, id_card.y0 + 19.0, id_card.x1 - 100.0, id_card.y1 - 6.0);
        if editing_signer {
            scene.stroke(&Stroke::new(1.5), base, theme.border_focus, None, &RoundedRect::from_rect(name_hit, theme.tokens.radii.sm as f64));
            *edit_rect = name_hit;
        }
        if name_shown.is_empty() {
            tr.draw_text(scene, "Your name", 13.0, theme.text_dim, base * Affine::translate((name_x, name_y)), None);
        } else {
            tr.draw_text_weighted(scene, name_shown, 13.0, theme.text_primary, base * Affine::translate((name_x, name_y)), Some((name_hit.width() - 6.0) as f32), TextWeight::Semibold);
        }
        if editing_signer {
            let caret = self.editing.as_ref().map(|e| e.caret).unwrap_or(0);
            draw_caret(scene, base, tr, theme.accent_primary, name_shown, 13.0, (name_hit.width() - 6.0) as f32, Point::new(name_x, name_y), caret);
        }
        hits.push((SidebarHit::EditSigner, name_hit));

        // Mini signature preview — the same style-from-name the placement
        // uses, so the card previews the exact stroke that will be stamped and
        // it updates live while the name is being edited.
        let prev = Rect::new(id_card.x1 - 96.0, id_card.y0 + 8.0, id_card.x1 - 12.0, id_card.y1 - 10.0);
        render::draw_signature_preview(scene, base, render::ink(), prev, signature_style_for(name_shown));
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

        // Nothing to report until a document is open.
        if !self.has_document {
            return;
        }

        let pad = 14.0;
        let size = 11.5f32;
        let ty = vcenter(s, size);

        // Center: page nav (always shown, it is the priority element).
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

        // Left: active tool + hint — only when it clears the page chevrons.
        let left = format!("{}  ·  {} key", self.tool.label(), self.tool.hotkey());
        let (llw, _) = tr.measure(&left, size, None);
        if s.x0 + pad + llw as f64 + 12.0 < prev.x0 {
            tr.draw_text(scene, &left, size, theme.text_dim, base * Affine::translate((s.x0 + pad, ty)), None);
        }

        // Right: markup counts — abbreviate as the bar narrows, and drop
        // entirely if even the short form would collide with the page label.
        let (a, cm, sg) = (self.annotations.len(), self.comments.len(), self.signatures.len());
        let full = format!("{a} annotations · {cm} comments · {sg} signatures");
        let (fw, _) = tr.measure(&full, size, None);
        let right = if s.x1 - pad - fw as f64 > next.x1 + 12.0 {
            Some(full)
        } else {
            let short = format!("{a} · {cm} · {sg}");
            let (sw, _) = tr.measure(&short, size, None);
            (s.x1 - pad - sw as f64 > next.x1 + 12.0).then_some(short)
        };
        if let Some(txt) = right {
            let (rw, _) = tr.measure(&txt, size, None);
            tr.draw_text(scene, &txt, size, theme.text_dim, base * Affine::translate((s.x1 - pad - rw as f64, ty)), None);
        }
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

/// A folder glyph within the given glyph `bounds` (already sized by the
/// caller — pass `icon_inset(button)` from the toolbar to match its peers).
fn draw_folder_glyph(scene: &mut Scene, base: Affine, bounds: Rect, color: Color) {
    let r = bounds.inflate(-1.5, -2.0);
    // Folder body with a raised tab on the top-left.
    let tab_w = r.width() * 0.44;
    let mut p = BezPath::new();
    p.move_to((r.x0, r.y1));
    p.line_to((r.x0, r.y0 + r.height() * 0.28));
    p.line_to((r.x0 + tab_w - 3.0, r.y0 + r.height() * 0.28));
    p.line_to((r.x0 + tab_w, r.y0 + r.height() * 0.1));
    p.line_to((r.x1, r.y0 + r.height() * 0.1));
    p.line_to((r.x1, r.y1));
    p.close_path();
    scene.stroke(&Stroke::new(1.6), base, color, None, &p);
}

/// A "save" glyph: a down arrow dropping into an open tray, within `bounds`.
fn draw_save_glyph(scene: &mut Scene, base: Affine, bounds: Rect, color: Color) {
    let r = bounds.inflate(-1.0, -1.0);
    let cx = (r.x0 + r.x1) / 2.0;
    // Down arrow (shaft + head) in the upper two-thirds.
    let top = r.y0 + 1.0;
    let mid = r.y0 + r.height() * 0.52;
    scene.stroke(&Stroke::new(1.6), base, color, None, &Line::new((cx, top), (cx, mid)));
    let head = {
        let mut p = BezPath::new();
        p.move_to((cx - r.width() * 0.22, mid - r.height() * 0.18));
        p.line_to((cx, mid));
        p.line_to((cx + r.width() * 0.22, mid - r.height() * 0.18));
        p
    };
    scene.stroke(&Stroke::new(1.6), base, color, None, &head);
    // Open tray along the bottom.
    let ty = r.y1 - r.height() * 0.14;
    let mut tray = BezPath::new();
    tray.move_to((r.x0 + 1.0, r.y1 - r.height() * 0.34));
    tray.line_to((r.x0 + 1.0, ty));
    tray.line_to((r.x1 - 1.0, ty));
    tray.line_to((r.x1 - 1.0, r.y1 - r.height() * 0.34));
    scene.stroke(&Stroke::new(1.6), base, color, None, &tray);
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

/// Draw a thin text caret after `text[..caret]`, wrapped the same way the
/// text itself is drawn. Uses the toolkit's measure convention (a wrapped
/// measure returns the *last line's* width and the total height), so the
/// caret lands where the next glyph would be typed on multi-line fields.
fn draw_caret(scene: &mut Scene, base: Affine, tr: &mut TextRenderer, color: Color, text: &str, size: f32, wrap: f32, origin: Point, caret: usize) {
    let caret = caret.min(text.len());
    let prefix = &text[..caret];
    let line_h = tr.measure("M", size, None).1.max(size) as f64;
    let (last_w, total_h) = tr.measure(prefix, size, Some(wrap));
    let n_lines = if line_h > 0.0 { (total_h as f64 / line_h).round().max(1.0) } else { 1.0 };
    let cx = if prefix.is_empty() { origin.x } else { origin.x + last_w as f64 };
    let cy = origin.y + (n_lines - 1.0) * line_h;
    let bar = Rect::new(cx, cy + 1.0, cx + 1.5, cy + line_h - 1.0);
    scene.fill(Fill::NonZero, base, color, None, &bar);
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
