// Included into `app.rs` (shares its module + imports). Holds the drawing
// routines and the pure layout helpers that both `draw` and event
// hit-testing rely on, so a click always lands where the pixel is.
//
// Everything around the pages is composed from the toolkit: the toolbar
// band, its buttons, segmented tool strip, swatches and zoom stepper are
// framework faces; find is the framework `FindBar`; the review panel stands
// on the inspector region and its tabs, rows, cards, avatars and buttons are
// library faces. Only the document itself (pages, markup, the selection and
// find highlights over its glyphs) is painted here, from theme values.

/// Measured heights for a single comment card (shared by scroll layout + draw).
struct CommentMetrics {
    card_h: f64,
    body_h: f64,
    reply_heights: Vec<f64>,
    /// Height contributed by the reply-compose row, including its top gap.
    compose_h: f64,
}

/// One comment card being painted: its rect, the comment, the metrics it
/// was measured with and the width of its text column.
struct CommentCard<'a> {
    rect: Rect,
    comment: &'a Comment,
    metrics: &'a CommentMetrics,
    text_w: f64,
}

/// Non-overlapping layout of a comment card's header row: the author's
/// avatar and name, then the Resolve button and the delete button, all on
/// one centre line. The name is bounded so a long name (or an email address)
/// elides instead of running under the buttons. Pure geometry so both the
/// draw pass and a test can assert the pieces never collide.
struct CommentHeader {
    avatar: Rect,
    /// Where the author's name is set (its band is the header row).
    name: Rect,
    resolve: Rect,
    delete: Rect,
    /// The left edge every text line of the card shares: the author's
    /// name, the body and the replies.
    text_x: f64,
    /// Top y where the card body begins, clear of the header row.
    body_top: f64,
}

/// Where a sidebar-tab draw pass accumulates its clickable regions and (if
/// something on that tab is being edited) the live inline-editor's screen
/// rect. Bundled so `draw_comments` / `draw_signatures` — which both write
/// to both — stay under the argument-count limit.
struct SidebarHits<'a> {
    hits: &'a mut Vec<(SidebarHit, Rect)>,
    edit_rect: &'a mut Rect,
}

struct ToolbarLayout {
    /// Toolbar rows this layout occupies: 1, or 2 when the tool strip no
    /// longer fits between the edge clusters and wraps beneath them.
    rows: usize,
    toggle_rail: Rect,
    open: Rect,
    save: Rect,
    /// The segmented tool strip, and each tool's segment inside it.
    tool_strip: Rect,
    tools: Vec<(Tool, Rect)>,
    /// Each swatch's pointer target; the painted swatch is centred in it.
    swatches: Vec<(Swatch, Rect)>,
    /// The zoom stepper, and its step-out / step-in ends.
    zoom: Rect,
    zoom_out: Rect,
    zoom_in: Rect,
    fit: Rect,
    search: Rect,
    toggle_sidebar: Rect,
}

/// The tool strip's segment labels, in `Tool::ALL` order.
fn tool_labels() -> Vec<&'static str> {
    Tool::ALL.iter().map(|tool| tool.label()).collect()
}

/// The Fit button's label: it names what pressing it does.
fn fit_label(fit: FitMode) -> &'static str {
    match fit {
        FitMode::Page => "Fit width",
        _ => "Fit page",
    }
}

/// The review panel's tab labels, full and short.
fn tab_labels(short: bool) -> [&'static str; 3] {
    if short {
        ["Notes", "Index", "Sign"]
    } else {
        ["Comments", "Contents", "Signatures"]
    }
}

/// A fresh zoom stepper: an inline `− 100 % +` control for the toolbar.
fn zoom_stepper(percent: f64, enabled: bool) -> ui::widget::NumberInput {
    ui::widget::NumberInput::new(percent)
        .with_range(ZOOM_MIN * 100.0, ZOOM_MAX * 100.0)
        .with_step(10.0)
        .with_suffix("%")
        .with_accessible_name("Zoom")
        .with_id("pdf-zoom")
        .compact()
        .with_enabled(enabled)
}

impl PdfApp {
    // ---- pure layout ------------------------------------------------

    fn toolbar_layout(&self) -> ToolbarLayout {
        self.toolbar_layout_for(self.toolbar_rect)
    }

    /// The toolbar laid out in `tb`. `recompute_regions` calls this with a
    /// one-row bar to learn how many rows the width needs; every other caller
    /// goes through `toolbar_layout` with the bar that was sized from it.
    fn toolbar_layout_for(&self, tb: Rect) -> ToolbarLayout {
        let theme = &self.layout_theme;
        let row_h = f64::from(theme.tokens.metrics.header_height);
        let control = ButtonFace::height(ControlSize::Regular, theme)
            .max(f64::from(theme.tokens.metrics.min_pointer_target));
        let inset = f64::from(theme.tokens.spacing.s3);
        let related = f64::from(theme.tokens.spacing.rhythm.related);
        let cluster = f64::from(theme.tokens.spacing.rhythm.section);
        // The edge clusters always live on the first row; `tb` may be two
        // rows tall, so centre on the row, not on the whole bar.
        let cy = tb.y0 + row_h / 2.0;
        let row =
            |x0: f64, width: f64| Rect::new(x0, cy - control / 2.0, x0 + width, cy + control / 2.0);

        // ---- trailing cluster, right to left ----
        let mut rx = tb.x1 - inset;
        let mut place = |width: f64, gap_after: f64| {
            let r = row(rx - width, width);
            rx -= width + gap_after;
            r
        };
        let toggle_sidebar = place(control, related);
        let search = place(control, cluster);
        let fit = place(self.fit_w, related);
        let zoom = place(self.zoom_w, 0.0);
        let right_start = zoom.x0;
        let zoom_out = Rect::new(zoom.x0, zoom.y0, zoom.x0 + zoom.height(), zoom.y1);
        let zoom_in = Rect::new(zoom.x1 - zoom.height(), zoom.y0, zoom.x1, zoom.y1);

        // ---- leading cluster ----
        let toggle_rail = row(tb.x0 + inset, control);
        let open = row(toggle_rail.x1 + cluster, control);
        let save = row(open.x1 + related, control);
        let left_end = save.x1;

        // ---- the tool strip, centred in the remaining span ----
        // When the span between the edge clusters cannot hold the strip, it
        // wraps onto a second row spanning the full width instead of running
        // under the zoom controls; every command stays reachable.
        let strip_w = self.tool_strip_w;
        let annotate = self.tool.annotates();
        let swatch_w = control * Swatch::ALL.len() as f64;
        // The strip keeps its place whichever tool is armed: room for the
        // swatches of a markup tool is kept beside it even while they are
        // hidden, so arming a tool never moves the others.
        let wanted = strip_w + cluster + swatch_w;
        let wrap = wanted + 2.0 * cluster > right_start - left_end;
        let (rows, tools_cy, span_x0, span_x1) = if wrap {
            (2, tb.y0 + row_h * 1.5, tb.x0 + inset, tb.x1 - inset)
        } else {
            (1, cy, left_end + cluster, right_start - cluster)
        };
        let strip_h = SegmentedFace::height(ControlSize::Regular, theme);
        let x0 = (f64::midpoint(span_x0, span_x1) - wanted / 2.0).max(span_x0);
        let tool_strip = Rect::new(
            x0,
            tools_cy - strip_h / 2.0,
            (x0 + strip_w).min(span_x1),
            tools_cy + strip_h / 2.0,
        );
        let n = Tool::ALL.len() as f64;
        let seg_w = tool_strip.width() / n;
        let tools = Tool::ALL
            .iter()
            .enumerate()
            .map(|(i, t)| {
                // Both edges from the same expression, so neighbours meet
                // exactly.
                let edge = |k: usize| tool_strip.x0 + seg_w * k as f64;
                (
                    *t,
                    Rect::new(edge(i), tool_strip.y0, edge(i + 1), tool_strip.y1),
                )
            })
            .collect();

        let mut swatches = Vec::new();
        let sx0 = tool_strip.x1 + cluster;
        if annotate && sx0 + swatch_w <= span_x1 {
            for (i, s) in Swatch::ALL.iter().enumerate() {
                let x = sx0 + control * i as f64;
                swatches.push((
                    *s,
                    Rect::new(
                        x,
                        tools_cy - control / 2.0,
                        x + control,
                        tools_cy + control / 2.0,
                    ),
                ));
            }
        }

        ToolbarLayout {
            rows,
            toggle_rail,
            open,
            save,
            tool_strip,
            tools,
            swatches,
            zoom,
            zoom_out,
            zoom_in,
            fit,
            search,
            toggle_sidebar,
        }
    }

    /// Measure the toolbar's text-sized controls (the tool strip, the Fit
    /// button and the zoom stepper) in `theme`, so the pure layout above
    /// places them at the width they paint at. Re-lays the regions out when a
    /// width changed (a theme or text-scale change).
    fn measure_toolbar(&mut self, tr: &mut TextRenderer, theme: &Theme) {
        let labels = tool_labels();
        let strip = SegmentedFace::intrinsic_width(&labels, ControlSize::Regular, tr, theme);
        let fit = ["Fit page", "Fit width"]
            .iter()
            .map(|label| {
                ButtonFace::new(Rect::ZERO, label)
                    .role(ButtonRole::Plain)
                    .intrinsic_width(tr, theme)
            })
            .fold(0.0, f64::max);
        let zoom = self.zoom_input.preferred_width(theme, tr);
        let changed = (strip - self.tool_strip_w).abs() > 0.5
            || (fit - self.fit_w).abs() > 0.5
            || (zoom - self.zoom_w).abs() > 0.5;
        if changed {
            self.tool_strip_w = strip;
            self.fit_w = fit;
            self.zoom_w = zoom;
            self.recompute_regions();
            self.sync_viewport_frame();
        }
    }

    /// Every tooltip-able toolbar button as `(label, anchor-rect)`. Used both
    /// to draw the hover popover and to resolve which button is under the
    /// pointer. Labels name the command and its keyboard shortcut.
    fn toolbar_tooltips(&self) -> Vec<(String, Rect)> {
        let lay = self.toolbar_layout();
        let mut out: Vec<(String, Rect)> = Vec::new();
        out.push((
            if self.show_rail {
                "Hide pages".to_string()
            } else {
                "Show pages".to_string()
            },
            lay.toggle_rail,
        ));
        out.push(("Open PDF (Ctrl+O)".to_string(), lay.open));
        out.push(("Save annotated PDF (Ctrl+S)".to_string(), lay.save));
        if self.has_document {
            for (t, r) in &lay.tools {
                out.push((format!("{} ({})", t.label(), t.hotkey()), *r));
            }
        }
        if self.tool.annotates() {
            for (s, r) in &lay.swatches {
                out.push((format!("{} marker", swatch_label(*s)), *r));
            }
        }
        out.push(("Find in document (Ctrl+F)".to_string(), lay.search));
        out.push((
            if self.show_sidebar {
                "Hide review panel (B)".to_string()
            } else {
                "Show review panel (B)".to_string()
            },
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
                if self.tooltip.label() == label {
                    self.tooltip_anchor = anchor;
                    // Advance time so the reveal delay can elapse across frames.
                    self.tooltip.tick(std::time::Duration::from_millis(120));
                } else {
                    // Fresh anchor — rebuild and restart the hover timer.
                    self.tooltip = Tooltip::new(label);
                    self.tooltip.on_hover_enter(std::time::Duration::ZERO);
                    self.tooltip_anchor = anchor;
                }
            }
            None => {
                if self.tooltip.label() != "" {
                    self.tooltip.on_hover_leave();
                    self.tooltip = Tooltip::new("");
                }
            }
        }
        // Animate only while a hover is still counting toward the reveal delay;
        // a shown or absent tooltip needs no further frames.
        self.tooltip_pending = hovering && !self.tooltip.is_visible();
    }

    /// The review panel's tab strip: three equal tabs across the top of the
    /// panel, one control tall.
    fn sidebar_tab_strip(&self) -> Rect {
        let theme = &self.layout_theme;
        let sb = self.sidebar_panel();
        let h = SegmentedFace::height(ControlSize::Regular, theme);
        let top = sb.y0 + f64::from(theme.tokens.spacing.s2);
        Rect::new(sb.x0, top, sb.x1, top + h)
    }

    fn sidebar_tab_rects(&self) -> [(SidebarTab, Rect); 3] {
        let strip = self.sidebar_tab_strip();
        let w = strip.width() / 3.0;
        let seg = |i: f64| {
            Rect::new(
                strip.x0 + w * i,
                strip.y0,
                strip.x0 + w * (i + 1.0),
                strip.y1,
            )
        };
        [
            (SidebarTab::Comments, seg(0.0)),
            (SidebarTab::Outline, seg(1.0)),
            (SidebarTab::Signatures, seg(2.0)),
        ]
    }

    /// The panel the review sidebar's contents stand on: the whole column
    /// when docked; inset from the window edges when it floats over a narrow
    /// window.
    fn sidebar_panel(&self) -> Rect {
        if self.overlay_mode {
            self.sidebar_rect
                .inset(-f64::from(self.layout_theme.tokens.spacing.s2))
        } else {
            self.sidebar_rect
        }
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
        // A page change (not the initial open) briefly floats the page bubble.
        if self.has_document
            && self.bubble_shown_page != 0
            && self.current_page != self.bubble_shown_page
        {
            self.page_bubble.poke(ui::time::now());
        }
        self.bubble_shown_page = self.current_page;
        self.measure_toolbar(tr, theme);
        if self.shots_selection {
            self.stage_shots_selection(tr);
        }

        // No window fill: the framework painted the content ground already.

        // A PDF page is a fixed-layout document: accessibility text scale
        // enlarges the reader chrome, not the page's coordinates. Rendering
        // its stored glyph positions at the chrome scale makes headings,
        // annotations and signatures collide instead of preserving the
        // document users are reviewing. Screen readers still get the complete
        // document semantics; zoom remains the explicit visual magnifier.
        let chrome_text_scale = tr.text_scale();
        tr.set_text_scale(1.0);
        // In text mode, refresh the page-point text-line cache for visible
        // pages so the smart highlight tool can snap to exactly the glyphs
        // being drawn. (Raster pages carry no toolkit text geometry.)
        if self.raster_active() {
            self.text_lines.clear();
            // Raster backend: refresh the real `PDFium` text layer for the
            // visible pages so selection + search overlays line up with glyphs.
            self.refresh_raster_text();
            self.refresh_raster_links();
        } else {
            self.refresh_text_lines(tr);
        }
        tr.set_text_scale(chrome_text_scale);

        // Resolve which toolbar button (if any) the pointer is hovering and
        // advance the tooltip reveal timer before the toolbar paints.
        self.resolve_tooltip();

        // Resolve sidebar scroll + any reveal request before painting it.
        self.prepare_sidebar_scroll(tr);

        // Resolve the page rail's geometry for this frame before rasterising, so
        // the thumbnail slots (which pages, at what width) are known.
        if self.show_rail {
            self.page_sidebar.set_aspect(self.ph() / self.pw());
            self.page_sidebar.set_marked(
                self.pages_with_markup()
                    .into_iter()
                    .map(|p| p - 1)
                    .collect(),
            );
            let cur = self.current_page.saturating_sub(1);
            self.page_sidebar.begin_frame_with_theme(
                theme,
                self.rail_rect,
                self.doc.page_count(),
                cur,
            );
            // Screenshot hook: stage a live reorder drag so a still frame shows
            // the floating ghost + the reflow gap. Never armed at runtime.
            if self.shots_rail_drag {
                let rects = self.page_sidebar.thumb_rects();
                if let (Some(first), Some(third)) = (rects.first(), rects.get(2)) {
                    let from = first.1.center();
                    let to = third.1.center();
                    self.page_sidebar.on_press(from);
                    self.page_sidebar.on_motion(to);
                    let _ = self.page_sidebar.take_events();
                }
            }
        }

        // Rasterise the pages + thumbnails on screen once, up front.
        let (page_imgs, thumb_imgs, page_tiles) = self.rasterize_visible();

        // Pages are fixed-layout (see above); the no-document surface is
        // reader chrome and keeps the chrome scale like the toolbar.
        if self.has_document {
            tr.set_text_scale(1.0);
        }
        self.draw_canvas(scene, base, tr, theme, &page_imgs, &page_tiles);
        self.draw_field_editor(scene, base, tr, theme);
        let free_text_edit = self.draw_free_text_editor(scene, base, tr, theme);
        tr.set_text_scale(chrome_text_scale);

        // The floating selection HUD over the page (shared `ui` component).
        self.draw_selection_hud(scene, base, tr, theme);
        self.draw_page_bubble(scene, base, tr, theme);
        if self.search_active {
            self.draw_find_bar(scene, base, tr, theme);
        }

        // On a narrow window the panels float above the canvas behind a scrim.
        if self.overlay_mode && (self.show_rail || self.show_sidebar) {
            ui::chrome::fill_scrim(
                scene,
                base,
                self.canvas_rect,
                ui::overlay::scrim::MODAL_STRENGTH,
                theme,
            );
        }

        self.draw_rail(scene, base, tr, theme, &thumb_imgs);
        let mut edit_rect = Rect::ZERO;
        let hits = self.draw_sidebar(scene, base, tr, theme, &mut edit_rect);
        self.edit_rect = edit_rect;
        if let Some(rect) = free_text_edit {
            self.edit_rect = rect;
        }
        self.draw_toolbar(scene, base, tr, theme);
        if self.shots_context_menu
            && self.context_menu.is_none()
            && let Some(annotation) = self.annotations.first()
        {
            let target = DeleteTarget::Annotation(annotation.id.clone());
            // The menu opens where a right-click on the markup would.
            let at = self
                .annotation_screen_anchor(annotation)
                .unwrap_or_else(|| self.canvas_rect.center());
            self.context_menu = Some(ContextMenu::delete(target, at, self.rect));
            self.shots_context_menu = false;
        }
        // Context menu + tooltip composite above everything else. File
        // open/save is the host desktop's portal picker (HIG §5), so there is
        // no in-app dialog to draw here.
        if let Some(menu) = self.context_menu.as_mut() {
            menu.widget
                .draw(scene, base, tr, theme, &PresenceCtx::empty());
        }
        self.draw_tooltip(scene, base, tr, theme);
        // A transient toast (save confirmation, copy confirmation) on top.
        self.draw_toast(scene, base, tr, theme);

        self.sidebar_hits = hits;
    }

    /// Where a menu for `annotation` opens on screen: under the first line
    /// of marked-up text, else at its anchor. `None` if its page is not laid
    /// out.
    fn annotation_screen_anchor(&self, annotation: &Annotation) -> Option<Point> {
        let origin = *self.page_origins().get(annotation.page.checked_sub(1)?)?;
        let z = self.zoom();
        let (x, y) = match &annotation.kind {
            AnnoKind::Markup { quads, .. } => quads
                .first()
                .map_or_else(|| annotation.anchor(), |q| (q.x, q.y + q.h)),
            _ => annotation.anchor(),
        };
        Some(Point::new(origin.x + x * z, origin.y + y * z))
    }

    /// Rasterise every page (and thumbnail) currently on screen exactly once,
    /// returning bitmaps keyed by 1-based page number. Cache hits inside the
    /// rasteriser make scrolling and panning allocation-free; only a zoom
    /// change or a page scrolling into view triggers real work. Empty in text
    /// mode.
    #[allow(
        clippy::too_many_lines,
        reason = "the draw-time request budget and synchronous compatibility cache share the same visibility pass"
    )]
    fn rasterize_visible(&mut self) -> RasterFrame {
        let mut page_imgs = std::collections::HashMap::new();
        let mut thumb_imgs = std::collections::HashMap::new();
        let mut page_tiles: std::collections::HashMap<usize, render::PageTiles> =
            std::collections::HashMap::new();
        self.pending_visuals = false;
        if !self.raster_active() {
            return (page_imgs, thumb_imgs, page_tiles);
        }

        let canvas = self.canvas_rect;
        let pageh = self.ph() * self.zoom();
        let card_w_px = (self.pw() * self.zoom()).max(1.0);
        // Render a little above display resolution so pages stay crisp when
        // the compositor scales the surface.
        let page_bucket = ((card_w_px * RASTER_SS).round() as u32).clamp(96, 4096);
        let mut want_pages = Vec::new();
        // Pages drawn too large for one bitmap are drawn from tiles; each
        // still gets a lower-resolution whole-page placeholder underneath.
        let mut tiled: Vec<crate::tiles::PagePlacement> = Vec::new();
        for (i, o) in self.page_origins().iter().enumerate() {
            if o.y > canvas.y1 || o.y + pageh < canvas.y0 {
                continue;
            }
            match self.tile_placement(i, *o) {
                Some(placement) => tiled.push(placement),
                None => want_pages.push(i),
            }
        }
        let placeholder_bucket =
            crate::source::Rasterizer::bucket_for(crate::tiles::PLACEHOLDER_PAGE_PX);
        let wanted_tiles =
            crate::tiles::wanted_tiles(&tiled, (canvas.x0, canvas.y0, canvas.x1, canvas.y1));
        for placement in &tiled {
            page_tiles.insert(
                placement.page + 1,
                render::PageTiles {
                    placement: *placement,
                    tiles: Vec::new(),
                },
            );
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
        #[cfg(test)]
        let mut skipped_renderable = false;
        #[cfg(test)]
        let mut rendered_new = false;
        if let Some(document) = self.raster_document.as_ref() {
            self.pending_visuals = want_pages.iter().any(|i| {
                !self
                    .raster_images
                    .contains_key(&(*i, crate::source::Rasterizer::bucket_for(page_bucket)))
            }) || want_thumbs.iter().any(|(i, width)| {
                !self
                    .raster_images
                    .contains_key(&(*i, crate::source::Rasterizer::bucket_for(*width)))
            });
            // Worker mode: never ask PDFium to rasterise from the draw pass.
            // A cache miss becomes bounded owner work and this frame simply
            // paints the existing placeholder; the result wakes a later frame.
            for i in want_pages {
                let key = (i, crate::source::Rasterizer::bucket_for(page_bucket));
                if let Some(image) = self.raster_images.get(&key) {
                    page_imgs.insert(i + 1, image.clone());
                } else if new_budget > 0 && self.raster_render_pending.insert(key) {
                    new_budget -= 1;
                    if self.raster_handle.as_ref().is_some_and(|handle| {
                        handle
                            .request_render(crate::source::RenderRequest {
                                document_id: document.id,
                                page: i,
                                target_width: page_bucket,
                            })
                            .is_ok()
                    }) {
                        // The next event turn will retry/paint it.
                    } else {
                        self.raster_render_pending.remove(&key);
                        self.status_note = Some("PDF render worker is unavailable".to_string());
                    }
                }
            }
            let mut thumb_budget = MAX_NEW_THUMB_RASTERS_PER_FRAME;
            for (i, tw) in want_thumbs {
                let key = (i, crate::source::Rasterizer::bucket_for(tw));
                if let Some(image) = self.raster_images.get(&key) {
                    thumb_imgs.insert(i + 1, image.clone());
                } else if thumb_budget > 0 && self.raster_render_pending.insert(key) {
                    thumb_budget -= 1;
                    if !self.raster_handle.as_ref().is_some_and(|handle| {
                        handle
                            .request_render(crate::source::RenderRequest {
                                document_id: document.id,
                                page: i,
                                target_width: tw,
                            })
                            .is_ok()
                    }) {
                        self.raster_render_pending.remove(&key);
                        self.status_note = Some("PDF render worker is unavailable".to_string());
                    }
                }
            }
            let document_id = document.id;
            self.request_tiled_pages(
                document_id,
                &tiled,
                &wanted_tiles,
                placeholder_bucket,
                &mut page_imgs,
                &mut page_tiles,
            );
            // The worker completion itself marks the widget dirty.  Do not
            // spin an animation frame while it is parsing/rasterising: that
            // wastes a core on a static loading surface and makes a hostile
            // page feel worse rather than more responsive.
            self.pending_raster = false;
            return (page_imgs, thumb_imgs, page_tiles);
        }
        #[cfg(test)]
        if let Some(rz) = self.raster.as_mut() {
            for i in want_pages {
                if !rz.is_page_cached(i, page_bucket) {
                    if new_budget == 0 {
                        skipped_renderable = true;
                        continue;
                    }
                    new_budget -= 1;
                    if rz
                        .render(i, page_bucket)
                        .map(|img| page_imgs.insert(i + 1, img))
                        .is_some()
                    {
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
                    if rz
                        .render(i, tw)
                        .map(|img| thumb_imgs.insert(i + 1, img))
                        .is_some()
                    {
                        rendered_new = true;
                    }
                } else if let Some(img) = rz.render(i, tw) {
                    thumb_imgs.insert(i + 1, img);
                }
            }
            // Zoomed pages: the placeholder, then a budget of new tiles in
            // priority order (visible nearest the centre first).
            for placement in &tiled {
                let i = placement.page;
                if rz.is_page_cached(i, placeholder_bucket) || new_budget > 0 {
                    if !rz.is_page_cached(i, placeholder_bucket) {
                        new_budget -= 1;
                        rendered_new = true;
                    }
                    if let Some(img) = rz.render(i, placeholder_bucket) {
                        page_imgs.insert(i + 1, img);
                    }
                } else {
                    skipped_renderable = true;
                    self.pending_visuals = true;
                }
            }
            let mut tile_budget = MAX_NEW_TILE_RASTERS_PER_FRAME;
            for tile in wanted_tiles.iter().filter(|t| t.visible) {
                let cached = rz.is_tile_cached(tile.page, tile.level, tile.col, tile.row);
                if !cached {
                    if tile_budget == 0 {
                        skipped_renderable = true;
                        self.pending_visuals = true;
                        continue;
                    }
                    tile_budget -= 1;
                }
                if let Some(img) = rz.render_tile(tile.page, tile.level, tile.col, tile.row) {
                    rendered_new |= !cached;
                    if let Some(entry) = page_tiles.get_mut(&(tile.page + 1)) {
                        entry.tiles.push((tile.region, img));
                    }
                }
            }
            rz.prune(page_bucket);
        }
        #[cfg(not(test))]
        #[allow(
            clippy::needless_return,
            reason = "the test-only fallback is cfg-elided in production"
        )]
        {
            let _ = (&tiled, &wanted_tiles, placeholder_bucket);
            self.pending_raster = false;
            return (page_imgs, thumb_imgs, page_tiles);
        }
        #[cfg(test)]
        // Schedule a follow-up frame to render the pages we skipped this one —
        // but only when *this* frame made forward progress (rendered a new page).
        // A page that persistently fails to render therefore can't wedge the
        // frame pump into a permanent ~60 Hz spin.
        #[cfg(test)]
        {
            self.pending_raster = skipped_renderable && rendered_new;
            (page_imgs, thumb_imgs, page_tiles)
        }
    }

    /// Where a visible page's pixels land if it is drawn from tiles: `None`
    /// when a whole-page bitmap at this zoom stays under
    /// [`crate::tiles::TILE_MIN_PAGE_PX`]. The placement follows the page's
    /// card mapping, so tiles line up with text and markup overlays.
    fn tile_placement(&self, i: usize, origin: Point) -> Option<crate::tiles::PagePlacement> {
        let size = self.display_page_size(i)?;
        let map = self.page_map(i + 1);
        let z = self.zoom();
        let device = map.scale * z * RASTER_SS;
        if size.0 * device <= crate::tiles::TILE_MIN_PAGE_PX || !(size.0 * device).is_finite() {
            return None;
        }
        let level = crate::tiles::level_for(device);
        let scale = f64::from(crate::tiles::level_scale(level));
        Some(crate::tiles::PagePlacement {
            page: i,
            size,
            level,
            origin: (origin.x + map.offset.0 * z, origin.y + map.offset.1 * z),
            screen_per_px: map.scale * z / scale,
        })
    }

    /// Displayed size (points, recorded rotation applied) of 0-based page `i`.
    fn display_page_size(&self, i: usize) -> Option<(f64, f64)> {
        #[cfg(test)]
        if let Some(rz) = self.raster.as_ref() {
            return (i < rz.page_count()).then(|| rz.page_size(i));
        }
        self.raster_document.as_ref()?.page_sizes.get(i).copied()
    }

    /// Worker mode for zoomed pages: draw the tiles already here over the
    /// best whole-page bitmap available, and hand the worker the missing
    /// tiles as one batch, most important first. The batch replaces the
    /// previous one, so tiles that scrolled out of view are never rendered
    /// ahead of the ones now on screen.
    fn request_tiled_pages(
        &mut self,
        document_id: u64,
        tiled: &[crate::tiles::PagePlacement],
        wanted: &[crate::tiles::WantedTile],
        placeholder_bucket: u32,
        page_imgs: &mut std::collections::HashMap<usize, PenikoImage>,
        page_tiles: &mut std::collections::HashMap<usize, render::PageTiles>,
    ) {
        for placement in tiled {
            let i = placement.page;
            let best = self
                .raster_images
                .iter()
                .filter(|((page, _), _)| *page == i)
                .max_by_key(|((_, bucket), _)| *bucket)
                .map(|((_, bucket), image)| (*bucket, image.clone()));
            let best_bucket = best.as_ref().map_or(0, |(bucket, _)| *bucket);
            if let Some((_, image)) = best {
                page_imgs.insert(i + 1, image);
            }
            // A thumbnail is only a stopgap; ask for the real placeholder.
            if best_bucket < placeholder_bucket {
                let key = (i, placeholder_bucket);
                if self.raster_render_pending.insert(key)
                    && !self.raster_handle.as_ref().is_some_and(|handle| {
                        handle
                            .request_render(crate::source::RenderRequest {
                                document_id,
                                page: i,
                                target_width: placeholder_bucket,
                            })
                            .is_ok()
                    })
                {
                    self.raster_render_pending.remove(&key);
                }
                self.pending_visuals = true;
            }
        }
        let mut batch = Vec::new();
        for tile in wanted {
            let key = UiTileKey {
                page: tile.page,
                level: tile.level,
                col: tile.col,
                row: tile.row,
            };
            if let Some(image) = self.raster_tiles.get(&key) {
                if tile.visible
                    && let Some(entry) = page_tiles.get_mut(&(tile.page + 1))
                {
                    entry.tiles.push((tile.region, image));
                }
            } else {
                self.pending_visuals |= tile.visible;
                if batch.len() < crate::tiles::MAX_TILE_BATCH {
                    batch.push(crate::source::TileRequest {
                        document_id,
                        page: tile.page,
                        level: tile.level,
                        col: tile.col,
                        row: tile.row,
                    });
                }
            }
        }
        if batch != self.raster_tile_batch {
            let submitted = self.raster_handle.as_ref().is_some_and(|handle| {
                handle
                    .request_tiles(crate::source::TileBatch {
                        document_id,
                        tiles: batch.clone(),
                    })
                    .is_ok()
            });
            if submitted {
                self.raster_tile_batch = batch;
            } else if !batch.is_empty() {
                self.status_note = Some("PDF render worker is unavailable".to_string());
            }
        }
    }

    /// Ask for the link areas of the pages currently in view (raster backend
    /// only). Links are cheap to read and must be ready before the pointer
    /// reaches them, so unlike the text layer they are fetched on idle and
    /// scroll frames too; each page is read once per document and plan.
    fn refresh_raster_links(&mut self) {
        let pageh = self.ph() * self.zoom();
        let canvas = self.canvas_rect;
        let visible: Vec<usize> = self
            .page_origins()
            .iter()
            .enumerate()
            .filter(|(_, o)| o.y <= canvas.y1 && o.y + pageh >= canvas.y0)
            .map(|(i, _)| i)
            .collect();
        for i in visible {
            self.ensure_page_links(i + 1);
        }
    }

    /// Populate `self.raster_text` with the engine text layer of the pages
    /// currently in view (raster backend only). The engine's own text-page cache
    /// makes repeated frames cheap; only a page scrolling into view does work.
    fn refresh_raster_text(&mut self) {
        if !self.raster_active() {
            return;
        }
        // The engine text layer only feeds the selection + search-hit overlays
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
        #[cfg(test)]
        let pageh = self.ph() * self.zoom();
        #[cfg(test)]
        let canvas = self.canvas_rect;
        #[cfg(test)]
        let origins = self.page_origins();
        if self.worker_raster_active() {
            let Some(document) = self.raster_document.as_ref() else {
                return;
            };
            let pageh = self.ph() * self.zoom();
            let canvas = self.canvas_rect;
            let origins = self.page_origins();
            for (i, o) in origins.iter().enumerate() {
                if o.y > canvas.y1 || o.y + pageh < canvas.y0 {
                    continue;
                }
                if !self.raster_text.contains_key(&(i + 1))
                    && self.raster_text_pending.insert(i)
                    && !self.raster_handle.as_ref().is_some_and(|handle| {
                        handle
                            .request_text(crate::source::TextRequest {
                                document_id: document.id,
                                page: i,
                            })
                            .is_ok()
                    })
                {
                    self.raster_text_pending.remove(&i);
                    self.status_note = Some("PDF text worker is unavailable".to_string());
                }
            }
            return;
        }
        #[cfg(not(test))]
        return;
        #[cfg(test)]
        {
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
                    let pt = self.text_for_card(i + 1, pt);
                    self.raster_text.insert(i + 1, pt);
                }
            }
        }
    }

    /// Populate `self.text_lines` for the pages currently in view. Cheap: the
    /// measurer caches per (text,size,width) so repeated frames hit the cache.
    fn refresh_text_lines(&mut self, tr: &mut TextRenderer) {
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
                let lines = render::collect_text_lines(tr, page);
                self.text_lines.insert(page_num, lines);
            }
        }
    }

    fn draw_tooltip(&self, scene: &mut Scene, base: Affine, tr: &mut TextRenderer, theme: &Theme) {
        if !self.tooltip.is_visible() || self.tooltip.label().is_empty() {
            return;
        }
        let label = self.tooltip.label().to_string();
        ui::widget::tooltip::draw_hover_tooltip(
            scene,
            base,
            tr,
            theme,
            self.tooltip_anchor,
            &label,
            self.rect,
        );
    }

    // ---- canvas -----------------------------------------------------

    /// The no-document surface: the library empty state, with the one action
    /// that gets a document on screen. While an open is in flight it names
    /// the wait and offers nothing; when the last open failed it says why in
    /// one bounded line (the full reason stays in the accessible error).
    fn draw_empty_state(
        &self,
        scene: &mut Scene,
        base: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        canvas: Rect,
    ) {
        let detail;
        let content = if self.pending_open.is_some() {
            EmptyStateContent {
                icon: Some(Icon::FileText),
                ..EmptyStateContent::new("Opening PDF…")
            }
        } else if let Some(err) = &self.load_error {
            let lower = err.to_ascii_lowercase();
            detail = if lower.contains("password") || lower.contains("encrypt") {
                "This PDF needs a password before it can be opened."
            } else if lower.contains("unavailable") || lower.contains("not a readable pdf") {
                "The file is unavailable or isn't a readable PDF."
            } else {
                "Check the file and try again."
            };
            EmptyStateContent {
                icon: Some(Icon::TriangleAlert),
                message: Some(detail),
                action_label: Some("Open another PDF"),
                error: true,
                ..EmptyStateContent::new("Couldn't open this document")
            }
        } else {
            EmptyStateContent {
                icon: Some(Icon::FileText),
                action_label: Some("Open PDF"),
                ..EmptyStateContent::new("No document open")
            }
        };
        let action = EmptyState::paint(
            &mut PaintCtx::new(scene, base, tr, theme),
            canvas,
            &content,
            self.last_mouse,
        );
        // The click handler reads this exact rect back
        // (`empty_open_button_rect`); nothing to click while opening.
        self.empty_open_button.set(action.unwrap_or(Rect::ZERO));
    }

    fn draw_canvas(
        &self,
        scene: &mut Scene,
        base: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        page_imgs: &std::collections::HashMap<usize, PenikoImage>,
        page_tiles: &std::collections::HashMap<usize, render::PageTiles>,
    ) {
        let canvas = self.canvas_rect;

        // No document open: a calm empty surface on the content ground.
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
            let Some(page) = self.doc.page(page_num) else {
                continue;
            };
            let found: Vec<render::FoundSpan> = self
                .matches
                .iter()
                .enumerate()
                .filter(|(_, m)| m.page == page_num)
                .map(|(index, m)| render::FoundSpan {
                    block: m.block,
                    start: m.start,
                    len: m.len,
                    current: index == self.active_match,
                })
                .collect();
            render::draw_page(
                &mut PaintCtx::new(scene, base, tr, theme),
                page,
                render::PageFrame {
                    origin: *origin,
                    scale: self.zoom(),
                    page_w: self.pw(),
                    page_h: self.ph(),
                },
                render::PageRaster {
                    image: page_imgs.get(&page_num),
                    tiles: page_tiles.get(&page_num),
                },
                &found,
                render::Markup {
                    annotations: &self.annotations,
                    signatures: &self.signatures,
                    selected: self.selected_anno.as_deref(),
                },
            );

            self.draw_import_placeholder(scene, base, tr, theme, *origin, i);
            self.draw_form_fields(scene, base, tr, theme, page_num, *origin);

            // Raster backend: draw search-hit + selection bands over the page
            // image, using the real glyph geometry so they land on the text.
            if self.raster_active() {
                self.draw_text_overlays(scene, base, theme, page_num, *origin);
            }
        }

        // Drag preview.
        if let Some(d) = &self.drag
            && let Some(origin) = origins.get(d.page.saturating_sub(1))
        {
            let col = render::swatch_color(self.swatch);
            match d.tool {
                Tool::Highlight => {
                    let r = PtRect::from_corners(d.start.0, d.start.1, d.cur.0, d.cur.1);
                    let scr = Rect::new(
                        origin.x + r.x * self.zoom(),
                        origin.y + r.y * self.zoom(),
                        origin.x + (r.x + r.w) * self.zoom(),
                        origin.y + (r.y + r.h) * self.zoom(),
                    );
                    render::draw_highlight_preview(scene, base, col, scr);
                }
                Tool::Ink if d.points.len() >= 2 => {
                    let points: Vec<Point> = d
                        .points
                        .iter()
                        .map(|p| {
                            Point::new(origin.x + p.0 * self.zoom(), origin.y + p.1 * self.zoom())
                        })
                        .collect();
                    render::draw_ink_preview(scene, base, col, &points, self.zoom());
                }
                _ => {}
            }
        }

        scene.pop_layer();

        // Vertical + horizontal scrollbars: painted by the shared viewport
        // kernel so they match the other document apps (they appear only when
        // the content overflows the viewport on that axis).
        self.viewport.draw_scrollbars(scene, base, theme);
    }

    /// Form widgets on a page: a quiet outline so fields are discoverable,
    /// and any value recorded in the document's record drawn over the PDF's
    /// own appearance (the export writes the same value into the file).
    fn draw_form_fields(
        &self,
        scene: &mut Scene,
        base: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        page: usize,
        origin: Point,
    ) {
        let z = self.zoom();
        for w in self.field_widgets_on(page) {
            let r = Rect::new(
                origin.x + w.rect.x * z,
                origin.y + w.rect.y * z,
                origin.x + (w.rect.x + w.rect.w) * z,
                origin.y + (w.rect.y + w.rect.h) * z,
            );
            if self.is_editing(&EditTarget::FormField(w.name.clone())) {
                continue;
            }
            let value = self
                .form_values
                .contains_key(&w.name)
                .then(|| match w.kind {
                    tilo_pdf::FieldKind::CheckBox | tilo_pdf::FieldKind::RadioButton => {
                        render::FieldValue::Check(self.field_checked(&w))
                    }
                    _ => render::FieldValue::Text(self.field_text(&w)),
                });
            render::draw_form_field(
                &mut PaintCtx::new(scene, base, tr, theme),
                r,
                z,
                value.as_ref(),
            );
        }
    }

    /// The inline editor over the free text box being edited, if any: typed
    /// in place, at the box's position, width and text size.
    fn draw_free_text_editor(
        &mut self,
        scene: &mut Scene,
        base: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) -> Option<Rect> {
        let Some(EditTarget::FreeText(id)) = self.editing.as_ref().map(|e| e.target.clone()) else {
            return None;
        };
        let (page, rect) =
            self.annotations
                .iter()
                .find(|a| a.id == id)
                .and_then(|a| match &a.kind {
                    AnnoKind::FreeText { rect, .. } => Some((a.page, *rect)),
                    _ => None,
                })?;
        let origins = self.page_origins();
        let origin = page.checked_sub(1).and_then(|i| origins.get(i)).copied()?;
        let z = self.zoom();
        let r = Rect::new(
            origin.x + rect.x * z,
            origin.y + rect.y * z,
            origin.x + (rect.x + rect.w) * z,
            origin.y + (rect.y + rect.h) * z,
        );
        render::draw_edit_box(scene, base, r);
        ui::widget::paint_focus_ring(scene, base, r, f64::from(theme.tokens.radii.detail), theme);
        if let Some(Editing {
            input: EditInput::Plain(input),
            ..
        }) = self.editing.as_mut()
        {
            draw_inline_editor(input, scene, base, tr, theme, r);
        }
        Some(r)
    }

    /// The inline editor over the text field being edited, if any.
    fn draw_field_editor(
        &mut self,
        scene: &mut Scene,
        base: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let Some(EditTarget::FormField(name)) = self.editing.as_ref().map(|e| e.target.clone())
        else {
            return;
        };
        let origins = self.page_origins();
        let z = self.zoom();
        let Some((origin, widget)) = origins.iter().enumerate().find_map(|(i, o)| {
            self.field_widgets_on(i + 1)
                .into_iter()
                .find(|w| w.name == name)
                .map(|w| (*o, w))
        }) else {
            return;
        };
        let r = Rect::new(
            origin.x + widget.rect.x * z,
            origin.y + widget.rect.y * z,
            origin.x + (widget.rect.x + widget.rect.w) * z,
            origin.y + (widget.rect.y + widget.rect.h) * z,
        );
        render::draw_edit_box(scene, base, r);
        ui::widget::paint_focus_ring(scene, base, r, f64::from(theme.tokens.radii.detail), theme);
        if let Some(Editing {
            input: EditInput::Plain(input),
            ..
        }) = self.editing.as_mut()
        {
            draw_inline_editor(input, scene, base, tr, theme, r);
        }
    }

    /// Cover a page-shaped imported occurrence whose verified blob has not
    /// arrived yet. Keeping this separate from ordinary page rendering makes
    /// the unavailable state impossible to mistake for nearby source content.
    #[cfg(test)]
    fn draw_import_placeholder(
        &self,
        scene: &mut Scene,
        base: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        origin: Point,
        page_index: usize,
    ) {
        let Some(state) = self
            .raster
            .as_ref()
            .and_then(|raster| raster.page_blob_state(page_index))
        else {
            return;
        };
        let label = match state {
            tilo_content::ContentState::Pending => "Imported page is loading…",
            tilo_content::ContentState::Unavailable | tilo_content::ContentState::Resolved => {
                "Imported page is unavailable"
            }
        };
        let page_rect = Rect::new(
            origin.x,
            origin.y,
            origin.x + self.pw() * self.zoom(),
            origin.y + self.ph() * self.zoom(),
        );
        EmptyState::paint(
            &mut PaintCtx::new(scene, base, tr, theme),
            page_rect,
            &EmptyStateContent {
                icon: Some(Icon::Image),
                ..EmptyStateContent::new(label)
            },
            Point::new(f64::NEG_INFINITY, f64::NEG_INFINITY),
        );
    }

    #[cfg(not(test))]
    #[allow(
        clippy::unused_self,
        reason = "the cfg-matched test renderer reads local fixture state"
    )]
    fn draw_import_placeholder(
        &self,
        _scene: &mut Scene,
        _base: Affine,
        _tr: &mut TextRenderer,
        _theme: &Theme,
        _origin: Point,
        _page_index: usize,
    ) {
        // Worker reconciliation deliberately leaves an honest page-shaped
        // placeholder until the imported source is materialised on its owner.
    }

    /// Draw the raster text-layer overlays for one page: every find match
    /// over exactly its glyphs (the current one emphasised) and the live
    /// selection in your selection tint. All rectangles come from the real
    /// engine glyph boxes, so they sit exactly over the printed characters.
    fn draw_text_overlays(
        &self,
        scene: &mut Scene,
        base: Affine,
        theme: &Theme,
        page_num: usize,
        origin: Point,
    ) {
        let z = self.zoom();
        let to_screen = |r: &PtRect| {
            Rect::new(
                origin.x + r.x * z,
                origin.y + r.y * z,
                origin.x + (r.x + r.w) * z,
                origin.y + (r.y + r.h) * z,
            )
        };

        // Image-region marquee (Shift + Select): your selection tint with a
        // dashed edge in your colour, drawn even when the page has no text.
        if let Some(region) = self.region
            && region.page == page_num
        {
            render::draw_region_marquee(scene, base, theme, to_screen(&region.rect()));
        }

        let Some(layer) = self.raster_text.get(&page_num) else {
            return;
        };

        // Find matches, one highlight per line a match covers.
        for (i, m) in self.raster_matches.iter().enumerate() {
            if m.page != page_num {
                continue;
            }
            let current = i == self.active_match;
            for band in layer.bands_for_range(m.char_start, m.char_start + m.char_len) {
                ui::widget::paint_find_match(scene, base, to_screen(&band), current, theme);
            }
        }

        // Live selection.
        if let Some(sel) = self.selection
            && sel.page == page_num
            && sel.is_active()
        {
            let (a, b) = sel.range();
            let bands: Vec<Rect> = layer.bands_for_range(a, b).iter().map(to_screen).collect();
            render::draw_selection_bands(scene, base, theme, &bands);
        }
    }

    /// The floating selection HUD — the shared `ui::SelectionHud`, identical to
    /// the ebook reader's bar: highlight swatches + Copy + Note over the live
    /// text selection. Rebuilt each frame and stored for the next press to
    /// hit-test. Only shown once the drag has settled (a live drag would make
    /// the bar chase the pointer) and never while the inline editor is open.
    fn draw_selection_hud(
        &mut self,
        scene: &mut Scene,
        base: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        self.sel_hud = None;
        let Some(sel) = self.selection else { return };
        if sel.dragging || !sel.is_active() || self.editing.is_some() {
            return;
        }
        let Some(bounds_rect) = self.selection_screen_bounds() else {
            return;
        };
        // The selection band is normally painted by `draw_text_overlays` (raster
        // backend). The block-model sample isn't drawn there, so paint it here
        // for the seeded screenshot path — with no PDF open there is no raster
        // text layer, so this only fires for a staged selection.
        if !self.raster_active() {
            let z = self.zoom();
            let origins = self.page_origins();
            if let Some(sel) = self.selection
                && let Some(origin) = origins.get(sel.page.saturating_sub(1))
            {
                let bands: Vec<Rect> = self
                    .selection_page_bands()
                    .iter()
                    .map(|r| {
                        Rect::new(
                            origin.x + r.x * z,
                            origin.y + r.y * z,
                            origin.x + (r.x + r.w) * z,
                            origin.y + (r.y + r.h) * z,
                        )
                    })
                    .collect();
                render::draw_selection_bands(scene, base, theme, &bands);
            }
        }
        // Anchor above the top edge of the selection, horizontally centred on it.
        let anchor = Point::new(
            f64::midpoint(bounds_rect.x0, bounds_rect.x1),
            bounds_rect.y0,
        );
        let c = &theme.tokens.colors;
        // The HUD is a floating Panel: the panel ground, its derived border
        // and the floating elevation; its ink is measured on that ground.
        let ground = ui::chrome::panel_ground(theme);
        let on_panel = theme.on_ground(ground);
        let style = ui::ReaderHudStyle {
            surface: ground,
            border: ui::chrome::panel_border(theme),
            elevation: theme.tokens.elevation.floating,
            text: on_panel.tokens.colors.text_primary,
            hover: c.lift_on(ground, 0.5),
            pill: c.lift_on(ground, 1.0),
            ring: c.intent.you,
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
            &ui::HudControls {
                swatches: &swatches,
                show_note: true,
                show_delete: false,
            },
            ui::HudPlacement {
                anchor,
                bounds: self.canvas_rect,
            },
            self.last_mouse,
        );
        self.sel_hud = Some(hud);
    }

    /// A transient toast (e.g. "Saved to …" or "Copied 42 characters")
    /// centred near the bottom of the canvas. Set on an action, cleared by the
    /// next document interaction (Esc, a new open/load).
    fn draw_toast(&self, scene: &mut Scene, base: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let Some(note) = self.status_note.as_deref() else {
            return;
        };
        if note.is_empty() {
            return;
        }
        paint_transient(
            scene,
            base,
            tr,
            theme,
            self.canvas_rect,
            ToastAnchor::BottomCenter,
            &ToastCard::new(note, ToastKind::Info),
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
        let pw = self.pw();

        // The shared page rail draws its region, the thumbnails' frames,
        // the current-page lift, page numbers, markup dots, drag reflow +
        // ghost, scrollbar, context menu and the import affordance. It renders
        // the page content through this closure — a real engine bitmap when
        // the raster backend has one, otherwise the block sketch. Move the
        // rail out of `self` so the closure can borrow the (immutable)
        // document alongside it.
        let mut sb = std::mem::replace(&mut self.page_sidebar, PageSidebar::new(1.0));
        let doc = &self.doc;
        sb.render(scene, base, tr, theme, |scene, _tr, base, i, rect| {
            let num = i + 1;
            if let Some(page) = doc.page(num) {
                render::Thumbnail {
                    page,
                    page_w: pw,
                    raster: thumb_imgs.get(&num),
                }
                .draw(scene, base, rect);
            }
        });
        self.page_sidebar = sb;
    }

    // ---- toolbar ----------------------------------------------------

    /// The window's toolbar row: no fill of its own (it stands on the content
    /// ground) and one hairline under it; every control is a framework face.
    fn draw_toolbar(
        &mut self,
        scene: &mut Scene,
        base: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let tb = self.toolbar_rect;
        ui::widget::toolbar::paint_band(scene, base, theme, tb);
        let lay = self.toolbar_layout();
        let hover = |r: Rect| r.contains(self.last_mouse);
        let has_doc = self.has_document;

        // Panel toggles and find are on while their panel is shown: the lift
        // the toolkit's toolbar paints under a toggle that is on.
        let ground = theme.tokens.colors.surfaces.content;
        for (rect, on) in [
            (lay.toggle_rail, self.show_rail),
            (lay.search, self.search_active),
            (lay.toggle_sidebar, self.show_sidebar),
        ] {
            if on {
                ui::chrome::paint_row_selection(
                    scene,
                    base,
                    rect,
                    SelectionState::active(true),
                    ground,
                    theme,
                );
            }
        }
        let buttons = [
            (
                lay.toggle_rail,
                Icon::PanelLeft,
                "Show or hide pages",
                !has_doc,
            ),
            (
                lay.open,
                Icon::FolderOpen,
                "Open PDF",
                self.pending_open.is_some(),
            ),
            (
                lay.save,
                Icon::Download,
                "Save annotated PDF",
                !(has_doc && self.raster_active()),
            ),
            (lay.search, Icon::Search, "Find in document", !has_doc),
            (
                lay.toggle_sidebar,
                Icon::PanelRight,
                "Show or hide the review panel",
                !has_doc,
            ),
        ];
        for (rect, icon, name, disabled) in buttons {
            let face = ButtonFace::icon_only(rect, icon, name)
                .hovered(hover(rect) && !disabled)
                .disabled(disabled);
            Button::paint(scene, base, tr, theme, &face);
        }
        // Unsaved page edits (reorder / delete / duplicate / import): the
        // Save button carries the one live dot.
        if self.pages_dirty && has_doc && self.raster_active() {
            // No toolkit face paints a bare live dot on a button yet; it is
            // the badge's dot size, in your colour.
            let r = f64::from(theme.tokens.spacing.s1);
            let centre = Point::new(lay.save.x1 - r, lay.save.y0 + r);
            scene.fill(
                Fill::NonZero,
                base,
                theme.tokens.colors.intent.you,
                None,
                &Circle::new(centre, r),
            );
        }

        self.draw_toolbar_controls(scene, base, tr, theme, &lay);
    }

    /// The toolbar's tool strip, marker swatches, zoom stepper and Fit.
    fn draw_toolbar_controls(
        &mut self,
        scene: &mut Scene,
        base: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        lay: &ToolbarLayout,
    ) {
        let has_doc = self.has_document;
        // The tool strip: a segmented control, inert until a document is open.
        let labels = tool_labels();
        let rects: Vec<Rect> = lay.tools.iter().map(|(_, r)| *r).collect();
        let selected = Tool::ALL.iter().position(|t| *t == self.tool).unwrap_or(0);
        let hovered = has_doc
            .then(|| rects.iter().position(|r| r.contains(self.last_mouse)))
            .flatten();
        if !has_doc {
            scene.push_layer(
                Fill::NonZero,
                BlendMode::default(),
                ui::widget::DISABLED_ALPHA,
                base,
                &lay.tool_strip.inflate(2.0, 2.0),
            );
        }
        ToggleGroup::paint(
            scene,
            base,
            tr,
            theme,
            &SegmentedFace::new(&rects, &labels, selected).hovered(hovered),
        );
        if !has_doc {
            scene.pop_layer();
        }

        // Marker colours, while a markup tool is armed.
        if self.tool.annotates() {
            let side = f64::from(theme.tokens.metrics.icon_md);
            for (s, target) in &lay.swatches {
                let swatch = Rect::from_center_size(target.center(), (side, side));
                ColorSwatchFace::new(
                    swatch,
                    render::swatch_color(*s),
                    format!("{} marker", swatch_label(*s)),
                )
                .target_rect(*target)
                .selected(*s == self.swatch)
                .hovered(target.contains(self.last_mouse))
                .paint(scene, base, tr, theme);
            }
        }

        // Zoom stepper and Fit.
        if self.zoom_input_enabled != has_doc {
            self.zoom_input = zoom_stepper(self.zoom() * 100.0, has_doc);
            self.zoom_input_enabled = has_doc;
        }
        if !self.zoom_input.is_editing() {
            self.zoom_input.set_value((self.zoom() * 100.0).round());
        }
        self.zoom_input.apply_layout_external(lay.zoom);
        self.zoom_input
            .draw(scene, base, tr, theme, &PresenceCtx::empty());
        let fit = ButtonFace::new(lay.fit, fit_label(self.fit))
            .role(ButtonRole::Plain)
            .hovered(has_doc && lay.fit.contains(self.last_mouse))
            .disabled(!has_doc);
        Button::paint(scene, base, tr, theme, &fit);
    }

    // ---- find bar ---------------------------------------------------

    /// The framework find band at the top of the page canvas.
    fn draw_find_bar(
        &mut self,
        scene: &mut Scene,
        base: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        self.find_bar.layout(self.search_rect, theme, tr);
        self.find_bar
            .paint(scene, base, tr, theme, &PresenceCtx::empty());
    }

    // ---- review panel -----------------------------------------------

    fn draw_sidebar(
        &mut self,
        scene: &mut Scene,
        base: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        edit_rect: &mut Rect,
    ) -> Vec<(SidebarHit, Rect)> {
        let mut hits = Vec::new();
        if !self.show_sidebar {
            return hits;
        }
        // Docked, the panel is the window's inspector region; floating over
        // a narrow window it is the floating material.
        let panel = self.sidebar_panel();
        let local = if self.overlay_mode {
            ui::widget::floating_surface::paint(scene, base, panel, theme);
            theme.on_ground(ui::chrome::panel_ground(theme))
        } else {
            ui::surface::region(scene, base, panel, Region::Inspector, theme);
            theme.in_region(Region::Inspector)
        };
        let theme = &local;
        let ground = theme.tokens.colors.surfaces.content;

        // Tabs.
        let tab_rects = self.sidebar_tab_rects();
        let rects: Vec<Rect> = tab_rects.iter().map(|(_, r)| *r).collect();
        let full = tab_labels(false);
        let short = SegmentedFace::intrinsic_width(&full, ControlSize::Regular, tr, theme)
            > self.sidebar_tab_strip().width();
        let labels = tab_labels(short);
        let selected = tab_rects
            .iter()
            .position(|(tab, _)| *tab == self.sidebar_tab)
            .unwrap_or(0);
        ToggleGroup::paint(
            scene,
            base,
            tr,
            theme,
            &SegmentedFace::new(&rects, &labels, selected)
                .style(SegmentedStyle::Tabs)
                .hovered(rects.iter().position(|r| r.contains(self.last_mouse))),
        );
        for (tab, r) in tab_rects {
            hits.push((SidebarHit::Tab(tab), r));
        }
        let strip = self.sidebar_tab_strip();
        ui::surface::separator(scene, base, panel.x0, panel.x1, strip.y1, 0.0, ground);

        let body = self.sidebar_body();
        // Everything pushed from here on is painted inside the `body` clip, so
        // its hit rect must be clipped the same way (see below).
        let body_hits_start = hits.len();
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, base, &body);
        match self.sidebar_tab {
            SidebarTab::Comments => self.draw_comments(
                scene,
                base,
                tr,
                theme,
                body,
                &mut SidebarHits {
                    hits: &mut hits,
                    edit_rect,
                },
            ),
            SidebarTab::Outline => self.draw_outline(scene, base, tr, theme, body, &mut hits),
            SidebarTab::Signatures => self.draw_signatures(
                scene,
                base,
                tr,
                theme,
                body,
                &mut SidebarHits {
                    hits: &mut hits,
                    edit_rect,
                },
            ),
        }
        scene.pop_layer();
        // Hit rect == painted rect. Cards, buttons and rows scrolled under the
        // tab strip (or off the bottom) were still pushed at their unclipped
        // positions; because `apply_sidebar` picks the smallest containing
        // rect, an invisible delete button scrolled under "Contents" would
        // beat the tab. Clip every body hit to the body and drop the ones
        // nothing of which remains visible.
        let clipped: Vec<(SidebarHit, Rect)> = hits
            .drain(body_hits_start..)
            .filter_map(|(hit, r)| {
                let r = r.intersect(body);
                (r.width() > 0.0 && r.height() > 0.0).then_some((hit, r))
            })
            .collect();
        hits.extend(clipped);
        // The inline editor's rect is routed before any tab or toolbar hit,
        // so it is clipped the same way: an editor scrolled under the tab
        // strip must not swallow the press meant for the tab covering it.
        let visible_edit = edit_rect.intersect(body);
        *edit_rect = if visible_edit.width() > 0.0 && visible_edit.height() > 0.0 {
            visible_edit
        } else {
            Rect::ZERO
        };
        hits
    }

    /// The scrollable body of the review panel (below the tab strip).
    fn sidebar_body(&self) -> Rect {
        let panel = self.sidebar_panel();
        let rule = f64::from(self.layout_theme.tokens.metrics.chrome_rule_px);
        Rect::new(
            panel.x0,
            self.sidebar_tab_strip().y1 + rule,
            panel.x1,
            panel.y1,
        )
    }

    /// The inset of the panel's content from its edges.
    fn panel_inset(&self) -> f64 {
        f64::from(self.layout_theme.tokens.spacing.s3)
    }

    /// The y where the comments list starts painting (before scroll): below
    /// the fixed summary line. Shared by the draw pass, `sidebar_item_layout`
    /// and the reveal-scroll origin so they agree.
    fn comments_list_top(&self, body: Rect) -> f64 {
        let theme = &self.layout_theme;
        let rhythm = &theme.tokens.spacing.rhythm;
        let caption = f64::from(theme.text(TextRole::Caption).line_height_px());
        body.y0 + f64::from(rhythm.sibling) + caption + f64::from(rhythm.sibling)
    }

    /// The width the Resolve button needs for its widest label.
    fn resolve_button_w(tr: &mut TextRenderer, theme: &Theme) -> f64 {
        ["Resolve", "Reopen"]
            .iter()
            .map(|label| {
                ButtonFace::new(Rect::ZERO, label)
                    .role(ButtonRole::Plain)
                    .control_size(ControlSize::Small)
                    .intrinsic_width(tr, theme)
            })
            .fold(0.0, f64::max)
    }

    /// Lay out a comment card's header row in the card's body rect so the
    /// avatar, author name, Resolve button and delete button never overlap
    /// regardless of the name's length, all centred on one line.
    fn comment_header_layout(body: Rect, resolve_w: f64, theme: &Theme) -> CommentHeader {
        let row_h = ButtonFace::height(ControlSize::Small, theme);
        let cy = body.y0 + row_h / 2.0;
        let related = f64::from(theme.tokens.spacing.rhythm.related);
        let side = AvatarSize::Small.side(theme);
        let avatar = Rect::new(body.x0, cy - side / 2.0, body.x0 + side, cy + side / 2.0);
        let delete = Rect::new(body.x1 - row_h, cy - row_h / 2.0, body.x1, cy + row_h / 2.0);
        let resolve = Rect::new(
            delete.x0 - f64::from(theme.tokens.spacing.rhythm.tight) - resolve_w,
            delete.y0,
            delete.x0 - f64::from(theme.tokens.spacing.rhythm.tight),
            delete.y1,
        );
        let text_x = avatar.x1 + related;
        let name = Rect::new(
            text_x,
            body.y0,
            (resolve.x0 - related).max(text_x),
            body.y0 + row_h,
        );
        CommentHeader {
            avatar,
            name,
            resolve,
            delete,
            text_x,
            body_top: body.y0 + row_h + f64::from(theme.tokens.spacing.rhythm.tight),
        }
    }

    /// The card's body rect for a card whose outer width is `card_w`, placed
    /// at the origin: the width its text is laid out to.
    fn comment_text_w(theme: &Theme, card_w: f64) -> f64 {
        let body = CardFace::new().body_rect(
            &mut TextRenderer::new(),
            theme,
            Rect::new(0.0, 0.0, card_w, 0.0),
        );
        let side = AvatarSize::Small.side(theme);
        (body.width() - side - f64::from(theme.tokens.spacing.rhythm.related)).max(1.0)
    }

    /// Measured heights for one comment card. Computed once and shared by the
    /// scroll layout and the draw pass so the two never disagree about how tall
    /// a card is (which would misplace every card below it).
    fn comment_metrics(
        &self,
        tr: &mut TextRenderer,
        theme: &Theme,
        cm: &Comment,
        card_w: f64,
    ) -> CommentMetrics {
        let rhythm = &theme.tokens.spacing.rhythm;
        let text_w = Self::comment_text_w(theme, card_w);
        let line = f64::from(theme.text(TextRole::Body).line_height_px());
        let label = f64::from(theme.text(TextRole::Label).line_height_px());
        let row_h = ButtonFace::height(ControlSize::Small, theme);
        // The body is the live editor while this comment is focused, else its
        // text as written (a line's height when empty, for the hint).
        let bh = match self.editing_field(&EditTarget::CommentBody(cm.id.clone())) {
            Some(field) => field.rect().height().max(line),
            None if cm.body.is_empty() => line,
            None => {
                self.comment_view_height(tr, comment_view_key(&cm.id, None), &cm.blocks, text_w)
            }
        };
        let mut inner = row_h + f64::from(rhythm.tight) + bh;
        let mut reply_heights = Vec::with_capacity(cm.replies.len());
        for r in &cm.replies {
            let rh = if r.body.is_empty() {
                line
            } else {
                self.comment_view_height(
                    tr,
                    comment_view_key(&cm.id, Some(&r.id)),
                    &r.blocks,
                    text_w,
                )
            };
            reply_heights.push(rh);
            inner += f64::from(rhythm.sibling) + label + f64::from(rhythm.tight) + rh;
        }
        // Reply-compose row: the Reply button, or the open field.
        let compose_h = f64::from(rhythm.related)
            + match self.editing_field(&EditTarget::Reply(cm.id.clone())) {
                Some(field) => {
                    field.rect().height().max(line) + 2.0 * f64::from(theme.tokens.spacing.s2)
                }
                None => row_h,
            };
        inner += compose_h;
        let card_h = CardFace::new().height_for_body(tr, theme, card_w, inner);
        CommentMetrics {
            card_h,
            body_h: bh,
            reply_heights,
            compose_h,
        }
    }

    /// The comment editor open on `target`, if it is the edit in progress.
    fn editing_field(&self, target: &EditTarget) -> Option<&CommentField> {
        match self.editing.as_ref() {
            Some(Editing {
                target: editing,
                input: EditInput::Rich(field),
            }) if editing == target => Some(field),
            _ => None,
        }
    }

    /// The height `blocks` take shown at `width`, measured with the view
    /// that draws them.
    fn comment_view_height(
        &self,
        tr: &mut TextRenderer,
        key: String,
        blocks: &[ui::widget::BlockContent],
        width: f64,
    ) -> f64 {
        // Only the type scale reaches layout; colours are the painting theme's.
        let theme = self.layout_theme.clone();
        let mut views = self.comment_views.borrow_mut();
        let view = views.entry(key).or_default();
        view.show(blocks);
        view.layout(Self::rich_text_rect(&theme, 0.0, 0.0, width), &theme, tr)
    }

    /// The rect a comment's rich text is laid out in so its glyphs start at
    /// `x` and wrap at `width`: the editor sets each line one field padding
    /// in from its rect.
    fn rich_text_rect(theme: &Theme, x: f64, y: f64, width: f64) -> Rect {
        let pad = f64::from(theme.padding_standard);
        Rect::new(x - pad, y, x + width + pad, y)
    }

    /// The width of a card in the comments list.
    fn comment_card_w(&self, body: Rect) -> f64 {
        (body.width() - 2.0 * self.panel_inset()).max(1.0)
    }

    /// Content-relative layout of the active sidebar list as
    /// `(id, top, height)` entries plus the total content height. Tops are
    /// measured from the same origin the draw routines use *before* the
    /// scroll offset is applied, so the reveal math lines up exactly.
    fn sidebar_item_layout(&self, tr: &mut TextRenderer) -> (Vec<(String, f64, f64)>, f64) {
        let theme = &self.layout_theme;
        let body = self.sidebar_body();
        let gap = f64::from(theme.tokens.spacing.rhythm.sibling);
        let mut items = Vec::new();
        match self.sidebar_tab {
            SidebarTab::Comments => {
                let list_origin = self.comments_list_top(body);
                let card_w = self.comment_card_w(body);
                let mut y = list_origin;
                for cm in &self.comments {
                    let card_h = self.comment_metrics(tr, theme, cm, card_w).card_h;
                    items.push((cm.id.clone(), y, card_h));
                    y += card_h + gap;
                }
                (items, y - list_origin)
            }
            SidebarTab::Signatures => {
                // The composer is fixed; only the placed rows scroll.
                let base = self.sig_list_top(body);
                let row_h = self.sig_row_h(tr);
                let mut y = base;
                for s in &self.signatures {
                    items.push((s.id.clone(), y, row_h));
                    y += row_h;
                }
                (items, y - base)
            }
            SidebarTab::Outline => {
                // Contents is a genuine navigable list. Keep its geometry here
                // with the other sidebar lists so wheel scrolling and
                // draw-time hit regions remain in lockstep.
                let row_h = ui::chrome::nav_row_height(theme);
                let top = self.outline_top(body);
                let mut y = top;
                for (index, _) in self.doc.outline.iter().enumerate() {
                    items.push((format!("outline:{index}"), y, row_h));
                    y += row_h;
                }
                (items, (y - top).max(0.0))
            }
        }
    }

    /// Where the contents list starts.
    fn outline_top(&self, body: Rect) -> f64 {
        body.y0 + f64::from(self.layout_theme.tokens.spacing.s2)
    }

    /// Resolve `sidebar_scroll`: honour a pending reveal request (scrolling a
    /// freshly created comment/signature into view) and clamp to the list's
    /// scroll range so a deleted tail can't leave the list scrolled off.
    fn prepare_sidebar_scroll(&mut self, tr: &mut TextRenderer) {
        if !self.show_sidebar {
            self.sidebar_scroll = 0.0;
            self.sidebar_reveal = None;
            return;
        }
        let body = self.sidebar_body();
        let list_origin = match self.sidebar_tab {
            SidebarTab::Comments => self.comments_list_top(body),
            SidebarTab::Signatures => self.sig_list_top(body),
            SidebarTab::Outline => self.outline_top(body),
        };
        let viewport = (body.y1 - list_origin).max(0.0);
        let (items, content_h) = self.sidebar_item_layout(tr);
        let max_scroll = (content_h - viewport).max(0.0);

        if let Some(id) = self.sidebar_reveal.take()
            && let Some((_, top, h)) = items.iter().find(|(iid, _, _)| iid == &id)
        {
            // `top` is measured from the same origin as `list_origin`; bring
            // the item just inside the viewport.
            let rel_top = top - list_origin;
            let rel_bot = rel_top + h;
            if rel_top < self.sidebar_scroll {
                self.sidebar_scroll = rel_top;
            } else if rel_bot > self.sidebar_scroll + viewport {
                self.sidebar_scroll = rel_bot - viewport;
            }
        }
        self.sidebar_scroll = self.sidebar_scroll.clamp(0.0, max_scroll);
    }

    fn draw_comments(
        &mut self,
        scene: &mut Scene,
        base: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        body: Rect,
        out: &mut SidebarHits,
    ) {
        let inset = self.panel_inset();
        let gap = f64::from(theme.tokens.spacing.rhythm.sibling);
        let card_w = self.comment_card_w(body);
        // Snapshot the comment cache so the loop can borrow `self.editing`
        // mutably (to draw the inline editor) without aliasing `self.comments`.
        let comments = self.comments.clone();

        let total = comments.len();
        let open = comments.iter().filter(|c| !c.resolved).count();
        if total == 0 {
            EmptyState::paint(
                &mut PaintCtx::new(scene, base, tr, theme),
                body.inset(-inset),
                &EmptyStateContent {
                    icon: Some(Icon::Pencil),
                    message: Some("Select text or use the Note tool to add one."),
                    ..EmptyStateContent::new("No comments yet")
                },
                self.last_mouse,
            );
            return;
        }
        let summary = match (total, open) {
            (1, 1) => "1 comment, open".to_string(),
            (1, _) => "1 comment, resolved".to_string(),
            (_, 0) => format!("{total} comments, all resolved"),
            _ => format!("{total} comments, {open} open"),
        };
        let caption = theme.text(TextRole::Caption);
        tr.draw_style(
            scene,
            &summary,
            theme.text_dim,
            base * Affine::translate((
                body.x0 + inset,
                body.y0 + f64::from(theme.tokens.spacing.rhythm.sibling),
            )),
            None,
            &caption,
        );

        // Scrollable region starts below the fixed summary line.
        let mut y = self.comments_list_top(body) - self.sidebar_scroll;
        let resolve_w = Self::resolve_button_w(tr, theme);
        for cm in &comments {
            let selected = self
                .selected_anno
                .as_deref()
                .is_some_and(|sa| cm.anno_id.as_deref() == Some(sa));
            let m = self.comment_metrics(tr, theme, cm, card_w);
            let card = Rect::new(body.x0 + inset, y, body.x0 + inset + card_w, y + m.card_h);
            out.hits.push((SidebarHit::Comment(cm.id.clone()), card));
            // Cull cards scrolled out of the body, but keep advancing `y`.
            if card.y1 < body.y0 || card.y0 > body.y1 {
                y += m.card_h + gap;
                continue;
            }

            // A comment is an object (it opens its markup on the page): a
            // card. The chosen thread is lifted like a pressed card.
            let face = CardFace::new()
                .pressable()
                .interaction(ui::widget::Interaction {
                    hovered: card.contains(self.last_mouse),
                    pressed: selected,
                    ..Default::default()
                });
            let ctx = &mut PaintCtx::new(scene, base, tr, theme);
            face.paint(ctx, card);
            let card_body = face.body_rect(ctx.text, ctx.theme, card);
            let hdr = Self::comment_header_layout(card_body, resolve_w, theme);
            self.draw_comment_header_row(ctx, cm, &hdr, out);
            let comment_card = CommentCard {
                rect: card_body,
                comment: cm,
                metrics: &m,
                text_w: Self::comment_text_w(theme, card_w),
            };
            let yy = self.draw_comment_body(ctx, &comment_card, &hdr, out);
            let yy = self.draw_comment_replies(ctx, &comment_card, &hdr, yy);
            self.draw_comment_composer(ctx, &comment_card, &hdr, yy, out);

            y += m.card_h + gap;
        }
    }

    /// A comment card's header row: the author's avatar and name, the
    /// Resolve button and the delete button, on one centre line.
    fn draw_comment_header_row(
        &self,
        ctx: &mut PaintCtx<'_>,
        cm: &Comment,
        hdr: &CommentHeader,
        out: &mut SidebarHits,
    ) {
        let theme = ctx.theme;
        AvatarFace::new(&cm.author, &cm.author).paint(ctx, hdr.avatar);
        let style = theme.text(TextRole::Label).strong();
        let name = ctx
            .text
            .elide_to_width_style(&cm.author, hdr.name.width() as f32, &style);
        let (_, h) = ctx.text.measure_style(&name, None, &style);
        ctx.text(
            &name,
            &style,
            theme.text_primary,
            Point::new(hdr.name.x0, hdr.name.center().y - f64::from(h) / 2.0),
            None,
        );

        let resolve = ButtonFace::new(hdr.resolve, if cm.resolved { "Reopen" } else { "Resolve" })
            .role(ButtonRole::Plain)
            .control_size(ControlSize::Small)
            .hovered(hdr.resolve.contains(self.last_mouse));
        Button::paint(ctx.scene, ctx.transform, ctx.text, theme, &resolve);
        out.hits
            .push((SidebarHit::Resolve(cm.id.clone()), hdr.resolve));

        let delete = ButtonFace::icon_only(hdr.delete, Icon::X, "Delete comment")
            .control_size(ControlSize::Small)
            .hovered(hdr.delete.contains(self.last_mouse));
        Button::paint(ctx.scene, ctx.transform, ctx.text, theme, &delete);
        out.hits.push((
            SidebarHit::DeleteComment(cm.id.clone()),
            delete.hit_rect(theme),
        ));
    }

    /// A comment card's body — the text as written, or the shared editor
    /// while it's focused (click to edit; caret while focused). Returns the
    /// running `y` where the replies start.
    fn draw_comment_body(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        comment_card: &CommentCard<'_>,
        hdr: &CommentHeader,
        out: &mut SidebarHits,
    ) -> f64 {
        let (scene, base, tr, theme) = (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
        let CommentCard {
            rect: card,
            comment: cm,
            metrics: m,
            text_w,
        } = *comment_card;
        let body_x = hdr.text_x;
        let body_top = hdr.body_top;
        let tight = f64::from(theme.tokens.spacing.rhythm.tight);
        let editing_body = self.is_editing(&EditTarget::CommentBody(cm.id.clone()));
        let body_hit =
            Rect::new(body_x, body_top, card.x1, body_top + m.body_h).inflate(tight, tight);
        if editing_body {
            ui::widget::paint_focus_ring(
                scene,
                base,
                body_hit,
                f64::from(theme.tokens.radii.item),
                theme,
            );
            *out.edit_rect = body_hit;
            // The SDK editor owns the text, caret and selection while this
            // comment is focused, set where the static body draws.
            if let Some(Editing {
                input: EditInput::Rich(field),
                ..
            }) = self.editing.as_mut()
            {
                field.layout(
                    Self::rich_text_rect(theme, body_x, body_top, text_w),
                    theme,
                    tr,
                );
                field.draw(scene, base, tr, theme, &PresenceCtx::empty());
            }
        } else if cm.body.is_empty() {
            tr.draw_style(
                scene,
                "Write a comment…",
                theme.text_tertiary,
                base * Affine::translate((body_x, body_top)),
                Some(text_w as f32),
                &theme.text(TextRole::Body),
            );
        } else {
            let mut views = self.comment_views.borrow_mut();
            let view = views.entry(comment_view_key(&cm.id, None)).or_default();
            view.show(&cm.blocks);
            view.set_text_color(cm.resolved.then_some(theme.text_dim));
            view.layout(
                Self::rich_text_rect(theme, body_x, body_top, text_w),
                theme,
                tr,
            );
            view.draw(scene, base, tr, theme);
        }
        out.hits
            .push((SidebarHit::EditComment(cm.id.clone()), body_hit));
        body_top + m.body_h
    }

    /// A comment card's replies: each author over their reply, on the same
    /// text edge as the comment. Returns the updated running `y`.
    fn draw_comment_replies(
        &self,
        ctx: &mut PaintCtx<'_>,
        comment_card: &CommentCard<'_>,
        hdr: &CommentHeader,
        mut yy: f64,
    ) -> f64 {
        let (scene, base, tr, theme) = (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
        let CommentCard {
            comment: cm,
            metrics: m,
            text_w,
            ..
        } = *comment_card;
        let rhythm = &theme.tokens.spacing.rhythm;
        let author_style = theme.text(TextRole::Label).strong();
        let label_h = f64::from(author_style.line_height_px());
        let x0 = hdr.text_x;
        for (r, reply_h) in cm.replies.iter().zip(m.reply_heights.iter()) {
            yy += f64::from(rhythm.sibling);
            tr.draw_style(
                scene,
                &r.author,
                theme.text_primary,
                base * Affine::translate((x0, yy)),
                Some(text_w as f32),
                &author_style,
            );
            yy += label_h + f64::from(rhythm.tight);
            let mut views = self.comment_views.borrow_mut();
            let view = views
                .entry(comment_view_key(&cm.id, Some(&r.id)))
                .or_default();
            view.show(&r.blocks);
            view.set_text_color(cm.resolved.then_some(theme.text_dim));
            view.layout(Self::rich_text_rect(theme, x0, yy, text_w), theme, tr);
            view.draw(scene, base, tr, theme);
            yy += reply_h;
        }
        yy
    }

    /// A comment card's reply composer: the live editor while composing a
    /// reply, or the Reply button that starts one.
    fn draw_comment_composer(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        comment_card: &CommentCard<'_>,
        hdr: &CommentHeader,
        yy: f64,
        out: &mut SidebarHits,
    ) {
        let (scene, base, tr, theme) = (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
        let CommentCard {
            rect: card,
            comment: cm,
            metrics: m,
            text_w,
        } = *comment_card;
        let composing = self.is_editing(&EditTarget::Reply(cm.id.clone()));
        let top = yy + f64::from(theme.tokens.spacing.rhythm.related);
        if composing {
            // A field on the card: the raised well with its edge, in your
            // colour's focus ring while it types.
            let pad = f64::from(theme.tokens.spacing.s2);
            let field = Rect::new(
                hdr.text_x - pad,
                top,
                card.x1,
                top + m.compose_h - (top - yy),
            );
            let radius = f64::from(theme.tokens.radii.control);
            ui::surface::card(
                scene,
                base,
                field,
                radius,
                theme
                    .tokens
                    .colors
                    .lift_on(theme.tokens.colors.surfaces.raised, 0.5),
            );
            ui::widget::paint_focus_ring(scene, base, field, radius, theme);
            *out.edit_rect = field;
            if let Some(Editing {
                input: EditInput::Rich(reply),
                ..
            }) = self.editing.as_mut()
            {
                let tx = hdr.text_x;
                let ty = field.y0 + pad;
                reply.layout(Self::rich_text_rect(theme, tx, ty, text_w - pad), theme, tr);
                reply.draw(scene, base, tr, theme, &PresenceCtx::empty());
            }
            out.hits
                .push((SidebarHit::ReplyField(cm.id.clone()), field));
        } else {
            let face = ButtonFace::new(Rect::ZERO, "Reply")
                .icon(Icon::Reply)
                .role(ButtonRole::Plain)
                .control_size(ControlSize::Small);
            let w = face.intrinsic_width(tr, theme);
            // A Plain button's label sits one pad in; pull the button back by
            // that pad so its label lines up with the text above it.
            let pad = f64::from(theme.tokens.spacing.s2);
            let rect = Rect::new(
                hdr.text_x - pad,
                top,
                hdr.text_x - pad + w,
                top + ButtonFace::height(ControlSize::Small, theme),
            );
            let face = ButtonFace::new(rect, "Reply")
                .icon(Icon::Reply)
                .role(ButtonRole::Plain)
                .control_size(ControlSize::Small)
                .hovered(rect.contains(self.last_mouse));
            Button::paint(scene, base, tr, theme, &face);
            out.hits.push((SidebarHit::ReplyField(cm.id.clone()), rect));
        }
    }

    fn draw_outline(
        &self,
        scene: &mut Scene,
        base: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        body: Rect,
        hits: &mut Vec<(SidebarHit, Rect)>,
    ) {
        if self.doc.outline.is_empty() {
            EmptyState::paint(
                &mut PaintCtx::new(scene, base, tr, theme),
                body.inset(-self.panel_inset()),
                &EmptyStateContent {
                    icon: Some(Icon::List),
                    ..EmptyStateContent::new("This document has no contents")
                },
                self.last_mouse,
            );
            return;
        }
        let row_h = ui::chrome::nav_row_height(theme);
        let mut y = self.outline_top(body) - self.sidebar_scroll;
        // The current section: the last entry at or before the current page.
        let current = self
            .doc
            .outline
            .iter()
            .rposition(|item| item.page <= self.current_page);
        for (index, item) in self.doc.outline.iter().enumerate() {
            let row = Rect::new(body.x0, y, body.x1, y + row_h);
            y += row_h;
            if row.y1 < body.y0 || row.y0 > body.y1 {
                continue;
            }
            let face = NavRowFace::new(row, &item.title)
                .depth(usize::from(item.level))
                .count(NavCount::Neutral(
                    u32::try_from(item.page).unwrap_or(u32::MAX),
                ))
                .state(SelectionState::new(
                    current == Some(index),
                    row.contains(self.last_mouse),
                    false,
                ));
            ui::chrome::paint_nav_row(scene, base, tr, theme, &face);
            hits.push((SidebarHit::Outline(item.page), row));
        }
    }

    /// Height of the signature compose well, which differs by mode (the draw
    /// pad is taller than the name field).
    fn sig_compose_h(&self) -> f64 {
        let theme = &self.layout_theme;
        let row = ButtonFace::height(ControlSize::Regular, theme);
        match self.sig_mode {
            SigMode::Draw => 3.0 * row,
            SigMode::Type => 2.0 * row,
        }
    }

    /// One placed-signature row.
    fn sig_row_h(&self, tr: &mut TextRenderer) -> f64 {
        ListRowFace::new("Signer")
            .secondary("Page 1")
            .height(tr, &self.layout_theme)
    }

    /// The y (before scroll) where the placed-signatures list begins — below
    /// the fixed composer. Kept in one place so `draw_signatures` and
    /// `sidebar_item_layout` never disagree about the composer's height.
    fn sig_list_top(&self, body: Rect) -> f64 {
        self.signature_geometry(body).list_top
    }

    /// Where every fixed part of the Signatures panel sits.
    fn signature_geometry(&self, body: Rect) -> SignatureGeometry {
        let theme = &self.layout_theme;
        let rhythm = &theme.tokens.spacing.rhythm;
        let inset = self.panel_inset();
        let x0 = body.x0 + inset;
        let x1 = body.x1 - inset;
        let heading = f64::from(theme.text(TextRole::Subheading).line_height_px());
        let control = ButtonFace::height(ControlSize::Regular, theme);
        let mut y = body.y0 + f64::from(rhythm.sibling);
        let heading_y = y;
        y += heading + f64::from(rhythm.related);
        let mode = Rect::new(
            x0,
            y,
            x1,
            y + SegmentedFace::height(ControlSize::Regular, theme),
        );
        y = mode.y1 + f64::from(rhythm.sibling);
        let compose = Rect::new(x0, y, x1, y + self.sig_compose_h());
        y = compose.y1 + f64::from(rhythm.sibling);
        let caption = f64::from(theme.text(TextRole::Caption).line_height_px());
        let hint_y = y;
        if self.tool == Tool::Sign {
            y += caption + f64::from(rhythm.related);
        }
        let place = Rect::new(x0, y, x1, y + control);
        y = place.y1 + f64::from(rhythm.group);
        let placed_y = y;
        y += heading + f64::from(rhythm.related);
        SignatureGeometry {
            heading_y,
            mode,
            compose,
            hint_y,
            place,
            placed_y,
            list_top: y,
        }
    }

    fn draw_signatures(
        &mut self,
        scene: &mut Scene,
        base: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        body: Rect,
        out: &mut SidebarHits,
    ) {
        let geo = self.signature_geometry(body);
        let heading = theme.text(TextRole::Subheading);
        tr.draw_style(
            scene,
            "Your signature",
            theme.text_primary,
            base * Affine::translate((geo.mode.x0, geo.heading_y)),
            None,
            &heading,
        );

        // Type / Draw: a segmented control.
        let half = geo.mode.width() / 2.0;
        let seg_rects = [
            Rect::new(geo.mode.x0, geo.mode.y0, geo.mode.x0 + half, geo.mode.y1),
            Rect::new(geo.mode.x0 + half, geo.mode.y0, geo.mode.x1, geo.mode.y1),
        ];
        let seg_labels = ["Type", "Draw"];
        let sel = usize::from(self.sig_mode == SigMode::Draw);
        ToggleGroup::paint(
            scene,
            base,
            tr,
            theme,
            &SegmentedFace::new(&seg_rects, &seg_labels, sel)
                .hovered(seg_rects.iter().position(|r| r.contains(self.last_mouse))),
        );
        out.hits
            .push((SidebarHit::SigMode(SigMode::Type), seg_rects[0]));
        out.hits
            .push((SidebarHit::SigMode(SigMode::Draw), seg_rects[1]));

        // The compose well: a name field (Type) or a draw pad (Draw).
        self.sig_pad_rect = Rect::ZERO;
        match self.sig_mode {
            SigMode::Type => {
                self.draw_signature_compose_type(scene, base, tr, theme, geo.compose, out);
            }
            SigMode::Draw => {
                self.draw_signature_compose_draw(scene, base, tr, theme, geo.compose, out);
            }
        }

        let ctx = &mut PaintCtx::new(scene, base, tr, theme);
        self.draw_signature_place_button(ctx, &geo, out);
        self.draw_placed_signatures_list(ctx, body, &geo, out);
    }

    /// The well the signature is composed in: a raised field on the panel,
    /// in your colour's focus ring while it is being written into.
    fn paint_compose_well(
        scene: &mut Scene,
        base: Affine,
        theme: &Theme,
        rect: Rect,
        active: bool,
    ) {
        let radius = f64::from(theme.tokens.radii.control);
        ui::surface::card(
            scene,
            base,
            rect,
            radius,
            theme.tokens.colors.surfaces.raised,
        );
        if active {
            ui::widget::paint_focus_ring(scene, base, rect, radius, theme);
        }
    }

    /// The "type your name" composer: the signer-name field (editable
    /// inline) beside a live script preview of the stamped signature.
    fn draw_signature_compose_type(
        &mut self,
        scene: &mut Scene,
        base: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        card: Rect,
        out: &mut SidebarHits,
    ) {
        let editing_signer = self.is_editing(&EditTarget::Signer);
        Self::paint_compose_well(scene, base, theme, card, editing_signer);
        let pad = f64::from(theme.tokens.spacing.s3);
        let caption = theme.text(TextRole::Caption);
        let caption_h = f64::from(caption.line_height_px());
        tr.draw_style(
            scene,
            "Signing as",
            theme.text_dim,
            base * Affine::translate((card.x0 + pad, card.y0 + f64::from(theme.tokens.spacing.s2))),
            None,
            &caption,
        );
        // The name takes the left half; the preview the right.
        let mid = f64::midpoint(card.x0, card.x1);
        let name_rect = Rect::new(
            card.x0 + pad,
            card.y0 + f64::from(theme.tokens.spacing.s2) + caption_h,
            mid,
            card.y1 - f64::from(theme.tokens.spacing.s2),
        );
        let name_shown: String = if editing_signer {
            self.editing.as_ref().map(Editing::text).unwrap_or_default()
        } else {
            self.signer_name.clone()
        };
        if editing_signer {
            *out.edit_rect = name_rect;
            if let Some(Editing {
                input: EditInput::Plain(input),
                ..
            }) = self.editing.as_mut()
            {
                draw_inline_editor(input, scene, base, tr, theme, name_rect);
            }
        } else {
            let body = theme.text(TextRole::Body).strong();
            let (shown, ink) = if name_shown.is_empty() {
                ("Your name".to_string(), theme.text_tertiary)
            } else {
                (
                    tr.elide_to_width_style(&name_shown, name_rect.width() as f32, &body),
                    theme.text_primary,
                )
            };
            let (_, h) = tr.measure_style(&shown, None, &body);
            tr.draw_style(
                scene,
                &shown,
                ink,
                base * Affine::translate((name_rect.x0, name_rect.center().y - f64::from(h) / 2.0)),
                None,
                &body,
            );
        }
        out.hits.push((SidebarHit::EditSigner, name_rect));
        // Live preview: the typed name in the script that will be stamped,
        // in the document's ink on its paper.
        let prev = Rect::new(mid, card.y0 + pad, card.x1 - pad, card.y1 - pad);
        render::draw_signature_ink(
            scene,
            tr,
            base,
            theme.text_primary,
            prev,
            &SignatureInk::Typed,
            &name_shown,
        );
    }

    /// The freehand draw pad: the person drags here to draw their signature,
    /// with a Clear button once there is something drawn.
    fn draw_signature_compose_draw(
        &mut self,
        scene: &mut Scene,
        base: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        card: Rect,
        out: &mut SidebarHits,
    ) {
        Self::paint_compose_well(scene, base, theme, card, self.sig_stroke.is_some());
        self.sig_pad_rect = card;
        let pad = f64::from(theme.tokens.spacing.s3);
        if self.sig_strokes.is_empty() && self.sig_stroke.is_none() {
            let hint = "Draw your signature here";
            let style = theme.text(TextRole::Body);
            let (hw, hh) = tr.measure_style(hint, None, &style);
            tr.draw_style(
                scene,
                hint,
                theme.text_tertiary,
                base * Affine::translate((
                    card.center().x - f64::from(hw) / 2.0,
                    card.center().y - f64::from(hh) / 2.0,
                )),
                None,
                &style,
            );
        } else {
            // The committed strokes plus the in-progress one.
            let mut strokes = self.sig_strokes.clone();
            if let Some(cur) = &self.sig_stroke {
                strokes.push(cur.clone());
            }
            render::draw_signature_ink(
                scene,
                tr,
                base,
                theme.text_primary,
                card.inset(-pad),
                &SignatureInk::Drawn { strokes },
                "",
            );
        }
        // Clear, at the pad's top trailing corner, once there is ink.
        if !self.sig_strokes.is_empty() {
            let probe = ButtonFace::new(Rect::ZERO, "Clear")
                .role(ButtonRole::Plain)
                .control_size(ControlSize::Small);
            let w = probe.intrinsic_width(tr, theme);
            let inset = f64::from(theme.tokens.spacing.rhythm.tight);
            let clr = Rect::new(
                card.x1 - inset - w,
                card.y0 + inset,
                card.x1 - inset,
                card.y0 + inset + ButtonFace::height(ControlSize::Small, theme),
            );
            let face = ButtonFace::new(clr, "Clear")
                .role(ButtonRole::Plain)
                .control_size(ControlSize::Small)
                .hovered(clr.contains(self.last_mouse));
            Button::paint(scene, base, tr, theme, &face);
            out.hits.push((SidebarHit::SigClear, clr));
        }
    }

    /// The button that arms the Sign tool so the next page click stamps the
    /// composed signature, with a one-line hint while it is armed.
    fn draw_signature_place_button(
        &self,
        ctx: &mut PaintCtx<'_>,
        geo: &SignatureGeometry,
        out: &mut SidebarHits,
    ) {
        let (scene, base, tr, theme) = (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
        let arming = self.tool == Tool::Sign;
        let ready = self.composed_ink().is_some();
        if arming {
            tr.draw_style(
                scene,
                "Click on a page to place your signature.",
                theme.text_dim,
                base * Affine::translate((geo.place.x0, geo.hint_y)),
                Some(geo.place.width() as f32),
                &theme.text(TextRole::Caption),
            );
        }
        let label = if arming { "Cancel" } else { "Place on page" };
        let face = ButtonFace::new(geo.place, label)
            .hovered(geo.place.contains(self.last_mouse))
            .disabled(!arming && !ready);
        Button::paint(scene, base, tr, theme, &face);
        out.hits.push((SidebarHit::AddSignature, geo.place));
    }

    /// The "Placed" heading and the scrollable list of signatures already
    /// placed on the document.
    fn draw_placed_signatures_list(
        &self,
        ctx: &mut PaintCtx<'_>,
        body: Rect,
        geo: &SignatureGeometry,
        out: &mut SidebarHits,
    ) {
        let theme = ctx.theme;
        let heading = theme.text(TextRole::Subheading);
        ctx.text(
            "Placed",
            &heading,
            theme.text_primary,
            Point::new(geo.place.x0, geo.placed_y),
            None,
        );
        let count = self.signatures.len().to_string();
        let caption = theme.text(TextRole::Caption).numeric();
        let (cw, ch) = ctx.text.measure_style(&count, None, &caption);
        let heading_h = f64::from(heading.line_height_px());
        ctx.text(
            &count,
            &caption,
            theme.text_tertiary,
            Point::new(
                geo.place.x1 - f64::from(cw),
                geo.placed_y + (heading_h - f64::from(ch)) / 2.0,
            ),
            None,
        );
        if self.signatures.is_empty() {
            ctx.text(
                "Placed signatures appear here.",
                &theme.text(TextRole::Body),
                theme.text_dim,
                Point::new(geo.place.x0, geo.list_top),
                Some(geo.place.width() as f32),
            );
            return;
        }
        // The placed list scrolls; the composer above stays fixed. Clip it to
        // its own band so a scrolled row never covers the composer.
        let band = Rect::new(body.x0, geo.list_top, body.x1, body.y1);
        ctx.scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            1.0,
            ctx.transform,
            &band,
        );
        let row_h = self.sig_row_h(ctx.text);
        let mut y = geo.list_top - self.sidebar_scroll;
        let delete = [RowAction {
            icon: Icon::Trash2,
            label: "Delete signature",
        }];
        let n = self.signatures.len();
        for (i, s) in self.signatures.iter().enumerate() {
            let row = Rect::new(body.x0, y, body.x1, y + row_h);
            y += row_h;
            if row.y1 < band.y0 || row.y0 > band.y1 {
                continue;
            }
            let selected = self.selected_sig.as_deref() == Some(s.id.as_str());
            let next_selected = self
                .signatures
                .get(i + 1)
                .is_some_and(|next| self.selected_sig.as_deref() == Some(next.id.as_str()));
            let page = format!("Page {}", s.page);
            let hovered = row.contains(self.last_mouse);
            let face = ListRowFace::new(&s.signer)
                .leading(RowLeading::Icon(Icon::PenTool))
                .secondary(&page)
                .meta(&s.date)
                .selected(selected)
                .actions(&delete)
                .interaction(ui::widget::Interaction {
                    hovered,
                    ..Default::default()
                });
            let face = if i + 1 < n {
                face.separator(next_selected)
            } else {
                face
            };
            let actions = face.action_rects(row, theme);
            let face =
                face.hovered_action(actions.iter().position(|r| r.contains(self.last_mouse)));
            face.paint(ctx, row);
            out.hits.push((SidebarHit::SigJump(i), row));
            if let Some(rect) = actions.first() {
                out.hits
                    .push((SidebarHit::DeleteSignature(s.id.clone()), *rect));
            }
        }
        ctx.scene.pop_layer();
    }

    // ---- floating page bubble ---------------------------------------

    /// The floating page-info overlay: a small `‹ Page X of Y ›` pill over
    /// the bottom-centre of the canvas. It appears on a page change (poked
    /// from `draw_all`) or while the pointer hovers the bottom-centre band,
    /// then fades. Its chevrons step pages; its rects are captured for hover
    /// and click.
    fn draw_page_bubble(
        &mut self,
        scene: &mut Scene,
        base: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        self.bubble_rect = Rect::ZERO;
        self.bubble_prev = Rect::ZERO;
        self.bubble_next = Rect::ZERO;
        if !self.has_document {
            return;
        }
        let now = ui::time::now();
        let canvas = self.canvas_rect;
        let label = format!("Page {} of {}", self.current_page, self.doc.page_count());
        let control = f64::from(theme.tokens.metrics.min_pointer_target);
        let (pill_w, pill_h) = ui::status_bubble::pill_size(&label, tr, theme, control);
        let cx = canvas.center().x;
        let by = canvas.y1 - f64::from(theme.tokens.spacing.rhythm.group) - pill_h;
        let rect = Rect::new(cx - pill_w / 2.0, by, cx + pill_w / 2.0, by + pill_h);

        // Reveal zone: a band across the bottom-centre of the canvas (plus the
        // pill itself), so approaching the bottom edge surfaces the indicator.
        let reach = f64::from(theme.tokens.spacing.s8);
        let zone = Rect::new(rect.x0 - reach, rect.y0 - reach, rect.x1 + reach, canvas.y1);
        let over = self.shots_page_bubble || zone.contains(self.last_mouse);
        self.page_bubble.set_hover(over, now);

        let alpha = self.page_bubble.alpha(now);
        if alpha <= 0.0 {
            return;
        }

        // Capture geometry for hover + click hit-testing (the chevron slots).
        self.bubble_rect = rect;
        self.bubble_prev = Rect::new(rect.x0, rect.y0, rect.x0 + control, rect.y1);
        self.bubble_next = Rect::new(rect.x1 - control, rect.y0, rect.x1, rect.y1);

        scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            alpha,
            base,
            &rect.inflate(reach, reach),
        );
        ui::status_bubble::draw_pill(scene, base, tr, theme, rect, &label);
        let ink = theme.on_ground(ui::chrome::panel_ground(theme));
        let icon = f64::from(theme.tokens.metrics.icon_sm);
        for (slot, glyph, enabled) in [
            (self.bubble_prev, Icon::ChevronLeft, self.current_page > 1),
            (
                self.bubble_next,
                Icon::ChevronRight,
                self.current_page < self.doc.page_count(),
            ),
        ] {
            let colour = if enabled && slot.contains(self.last_mouse) {
                ink.text_primary
            } else if enabled {
                ink.text_dim
            } else {
                ink.text_tertiary
            };
            glyph.draw(
                scene,
                base,
                Rect::from_center_size(slot.center(), (icon, icon)),
                colour,
            );
        }
        scene.pop_layer();
    }
}

/// Where the fixed parts of the Signatures panel sit.
struct SignatureGeometry {
    heading_y: f64,
    mode: Rect,
    compose: Rect,
    hint_y: f64,
    place: Rect,
    placed_y: f64,
    list_top: f64,
}

// ----------------------------------------------------------------------
// Free-standing helpers
// ----------------------------------------------------------------------

fn swatch_label(s: Swatch) -> &'static str {
    match s {
        Swatch::Yellow => "Yellow",
        Swatch::Green => "Green",
        Swatch::Pink => "Pink",
        Swatch::Blue => "Blue",
        Swatch::Purple => "Purple",
        Swatch::Custom(_) => "Custom",
    }
}

/// Position + draw the shared inline `TextInput` over `rect` (the box it
/// edits), its text inset by the field's own padding so the first glyph sits
/// where the static text it replaces did.
fn draw_inline_editor(
    input: &mut TextInput,
    scene: &mut Scene,
    base: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
) {
    input.apply_layout_external(rect);
    input.draw(scene, base, tr, theme, &PresenceCtx::empty());
}
