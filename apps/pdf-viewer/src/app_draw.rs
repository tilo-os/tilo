// Included into `app.rs` (shares its module + imports). Holds the drawing
// routines and the pure layout helpers that both `draw` and event
// hit-testing rely on, so a click always lands where the pixel is.
//
// The reader's controls are the window bar's slots (`app/window_bar.rs`) and
// find is the window bar's search; the review panel stands on the inspector
// region and its tabs, rows, cards, avatars and buttons are library faces. Only the document itself (pages, markup, the selection and
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

/// The glyph a tool's toggle shows.
fn tool_icon(tool: Tool) -> Icon {
    match tool {
        Tool::Select => Icon::MousePointer2,
        Tool::Highlight => Icon::Highlighter,
        Tool::Ink => Icon::Pencil,
        Tool::Note => Icon::MessageSquare,
        Tool::Text => Icon::Type,
        Tool::Sign => Icon::PenTool,
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

impl PdfApp {
    // ---- pure layout ------------------------------------------------

    /// Measure the review panel's tabs in `theme`, so the pure layout
    /// places them at the width they paint at.
    fn measure_tabs(&mut self, tr: &mut TextRenderer, theme: &Theme) {
        // Tabs pack from the leading edge, each as wide as its label.
        let tab_widths = [false, true].map(|short| {
            let segments = tab_labels(short).map(Segment::new);
            let rects = SegmentedFace::partition_segments(
                Rect::new(0.0, 0.0, f64::MAX / 4.0, 1.0),
                &segments,
                SegmentedStyle::Tabs,
                ControlSize::Regular,
                tr,
                theme,
            );
            [0, 1, 2].map(|i| rects.get(i).map_or(0.0, Rect::width))
        });
        self.tab_widths = Some(tab_widths);
    }

    /// The review panel's tab strip: across the top of the panel, one
    /// control tall, its first label on the panel content's edge.
    fn sidebar_tab_strip(&self) -> Rect {
        let theme = &self.layout_theme;
        let sb = self.sidebar_panel();
        let h = SegmentedFace::height(ControlSize::Regular, theme);
        let top = sb.y0 + f64::from(theme.tokens.spacing.s2);
        let inset = self.panel_inset();
        Rect::new(sb.x0 + inset, top, sb.x1 - inset, top + h)
    }

    /// Whether the tabs use their short labels: the full ones do not fit.
    fn sidebar_tabs_short(&self) -> bool {
        self.tab_widths
            .is_some_and(|[full, _]| full.iter().sum::<f64>() > self.sidebar_tab_strip().width())
    }

    /// The tabs packed from the strip's leading edge, each as wide as its
    /// label (equal thirds until the labels are measured).
    fn sidebar_tab_rects(&self) -> [(SidebarTab, Rect); 3] {
        let strip = self.sidebar_tab_strip();
        let widths = match self.tab_widths {
            Some([full, short]) => {
                if self.sidebar_tabs_short() {
                    short
                } else {
                    full
                }
            }
            None => [strip.width() / 3.0; 3],
        };
        let mut x = strip.x0;
        [
            SidebarTab::Comments,
            SidebarTab::Outline,
            SidebarTab::Signatures,
        ]
        .into_iter()
        .zip(widths)
        .map(|(tab, w)| {
            let rect = Rect::new(x, strip.y0, (x + w).min(strip.x1), strip.y1);
            x = rect.x1;
            (tab, rect)
        })
        .collect::<Vec<_>>()
        .try_into()
        .unwrap_or([
            (SidebarTab::Comments, Rect::ZERO),
            (SidebarTab::Outline, Rect::ZERO),
            (SidebarTab::Signatures, Rect::ZERO),
        ])
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
        self.canvas_hover_zones.clear();
        self.current_page = self.nearest_page();
        // A page or zoom change (not the opening) says itself in the bar.
        self.follow_page_line();
        self.measure_tabs(tr, theme);
        // A key (a tool's hotkey, B, Ctrl+plus) changes what the window bar
        // shows without a sync.
        ui::window_toolbar::declare(Some(self.window_toolbar()));
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
        // reader chrome and keeps the chrome scale.
        if self.has_document {
            tr.set_text_scale(1.0);
        }
        self.draw_canvas(scene, base, tr, theme, &page_imgs, &page_tiles);
        self.draw_field_editor(scene, base, tr, theme);
        let free_text_edit = self.draw_free_text_editor(scene, base, tr, theme);
        tr.set_text_scale(chrome_text_scale);

        // The floating selection HUD over the page (shared `ui` component).
        self.draw_selection_hud(scene, base, tr, theme);
        // On a touchscreen, the grips that move either end of the selection.
        if let Some(grips) = self.selection_grips() {
            grips.draw(scene, base, theme);
        }

        // On a narrow window the panels float above the canvas behind a scrim.
        if self.overlay_mode && (self.show_rail || self.show_sidebar) {
            ui::chrome::fill_scrim(
                scene,
                base,
                self.canvas_rect,
                theme.tokens.colors.surfaces.scrim_strength,
                theme,
            );
        }

        self.draw_rail(scene, base, tr, theme, &thumb_imgs);
        let mut edit_rect = Rect::ZERO;
        let hits = self.draw_sidebar(scene, base, tr, theme, &mut edit_rect);
        if self.show_sidebar && !self.overlay_mode {
            self.review.paint_resize(scene, base, theme);
        }
        self.edit_rect = edit_rect;
        if let Some(rect) = free_text_edit {
            self.edit_rect = rect;
        }
        self.stage_shots_context_menu();
        // The context menu paints in the top layer (`draw_overlay`). File
        // open/save is the host desktop's portal picker (HIG §5), so there is
        // no in-app dialog to draw here.

        self.sidebar_hits = hits;
        self.shown.note(self.canvas_shown(base, theme));
    }

    /// What this frame shows of the document and what floats over it, for
    /// [`Widget::collect_damage`] to tell a scroll-only frame from the rest.
    fn canvas_shown(&self, base: Affine, theme: &Theme) -> CanvasShown {
        CanvasShown {
            scroll: (self.scroll_x(), self.scroll_y()),
            zoom: self.zoom(),
            canvas: self.canvas_rect,
            rect: self.rect,
            transform: ui::scroll_copy::on_screen(base),
            current_page: self.current_page,
            thumbs: [
                self.viewport.v_scrollbar_thumb(),
                self.viewport.h_scrollbar_thumb(),
            ],
            hud: self.sel_hud.as_ref().map_or(Rect::ZERO, |hud| {
                ui::widget::floating_surface::painted_bounds(hud.rect(), theme)
            }),
            floating: self.editing.is_some()
                || self.context_menu.is_some()
                || (self.overlay_mode && (self.show_rail || self.show_sidebar)),
        }
    }

    /// Screenshot hook: open the delete menu of the first markup, as a
    /// right-click at its start would, once the sync has populated it.
    fn stage_shots_context_menu(&mut self) {
        if !self.shots_context_menu || self.context_menu.is_some() {
            return;
        }
        let Some(annotation) = self.annotations.first() else {
            return;
        };
        let target = DeleteTarget::Annotation(annotation.id.clone());
        let at = self
            .delete_target_screen_rect(&target)
            .map_or_else(|| self.canvas_rect.center(), |r| r.origin());
        let anchor = self.delete_menu_anchor(&target, at);
        self.context_menu = Some(ContextMenu::delete(target, anchor, self.rect));
        self.shots_context_menu = false;
    }

    /// What a delete menu acts on, on screen: the whole marked-up text, the
    /// note's glyph, the ink's or text box's extent, or the placed
    /// signature. `None` if it has no place on a laid-out page.
    fn delete_target_screen_rect(&self, target: &DeleteTarget) -> Option<Rect> {
        let (page, r) = match target {
            DeleteTarget::Annotation(id) => {
                let a = self.annotations.iter().find(|a| &a.id == id)?;
                let r = match &a.kind {
                    AnnoKind::Markup { quads, .. } => quads
                        .iter()
                        .map(|q| Rect::new(q.x, q.y, q.x + q.w, q.y + q.h))
                        .reduce(|a, b| a.union(b))?,
                    AnnoKind::Ink { strokes } => strokes
                        .iter()
                        .flatten()
                        .map(|&(x, y)| Rect::new(x, y, x, y))
                        .reduce(|a, b| a.union(b))?,
                    AnnoKind::FreeText { rect, .. } => {
                        Rect::new(rect.x, rect.y, rect.x + rect.w, rect.y + rect.h)
                    }
                    AnnoKind::Note { x, y } => {
                        let half = f64::from(self.layout_theme.tokens.metrics.icon_md) / 2.0;
                        Rect::new(x - half, y - half, x + half, y + half)
                    }
                };
                (a.page, r)
            }
            DeleteTarget::Signature(id) => {
                let s = self.signatures.iter().find(|s| &s.id == id)?;
                let r = s.rect;
                (s.page, Rect::new(r.x, r.y, r.x + r.w, r.y + r.h))
            }
            DeleteTarget::Comment(_) => return None,
        };
        let origin = *self.page_origins().get(page.checked_sub(1)?)?;
        let z = self.zoom();
        Some(Rect::new(
            origin.x + r.x0 * z,
            origin.y + r.y0 * z,
            origin.x + r.x1 * z,
            origin.y + r.y1 * z,
        ))
    }

    /// Where the delete menu for `target`, asked for at `at`, opens: under
    /// what it acts on, from the pointer's column, so it never covers it
    /// (above it when there is no room below).
    fn delete_menu_anchor(&self, target: &DeleteTarget, at: Point) -> Rect {
        self.delete_target_screen_rect(target).map_or_else(
            || Rect::new(at.x, at.y, at.x, at.y),
            |r| {
                let x = at.x.clamp(r.x0, r.x1);
                Rect::new(x, r.y0, x, r.y1)
            },
        )
    }

    /// The bitmaps for every page (and thumbnail) currently on screen, keyed
    /// by 1-based page number, from what the PDF worker has delivered, and
    /// the worker's next renders asked for. Nothing here waits: a page is
    /// drawn from the best bitmap held for it (another width scaled, while
    /// the sharp one renders), and the worker's queue is replaced by what
    /// this frame wants, most important first: the pages on screen, the
    /// pages the scroll is heading into, then rail thumbnails. Empty when no
    /// PDF is open.
    fn rasterize_visible(&mut self) -> RasterFrame {
        let mut page_imgs = std::collections::HashMap::new();
        let mut thumb_imgs = std::collections::HashMap::new();
        let mut page_tiles: std::collections::HashMap<usize, render::PageTiles> =
            std::collections::HashMap::new();
        self.pending_visuals = false;
        let Some(document_id) = self.raster_document.as_ref().map(|document| document.id) else {
            return (page_imgs, thumb_imgs, page_tiles);
        };
        self.raster_images.begin_frame();

        let canvas = self.canvas_rect;
        self.raster_heading.follow(self.scroll_y());
        let card_w_px = (self.pw() * self.zoom()).max(1.0);
        // Render a little above display resolution so pages stay crisp when
        // the compositor scales the surface.
        let page_bucket = crate::source::RasterBackend::bucket_for(
            ((card_w_px * RASTER_SS).round() as u32).clamp(96, 4096),
        );
        let PagesInReach {
            visible,
            prefetch,
            tiled,
        } = self.pages_in_reach(self.raster_heading.reach(canvas.height()));

        let placeholder_bucket =
            crate::source::RasterBackend::bucket_for(crate::tiles::PLACEHOLDER_PAGE_PX);
        let wanted_tiles =
            crate::tiles::wanted_tiles(&tiled, (canvas.x0, canvas.y0, canvas.x1, canvas.y1));

        // Whole-page renders this frame wants, most important first.
        let mut wanted: Vec<(usize, u32)> = Vec::new();
        for (i, _) in &visible {
            match self.raster_images.best(*i, page_bucket) {
                Some((bucket, image)) => {
                    page_imgs.insert(i + 1, image);
                    if bucket != page_bucket {
                        wanted.push((*i, page_bucket));
                    }
                }
                None => wanted.push((*i, page_bucket)),
            }
        }
        for placement in &tiled {
            let i = placement.page;
            let best = self.raster_images.best(i, u32::MAX);
            let best_bucket = best.as_ref().map_or(0, |(bucket, _)| *bucket);
            if let Some((_, image)) = best {
                page_imgs.insert(i + 1, image);
            }
            // A thumbnail is only a stand-in; ask for the real placeholder.
            if best_bucket < placeholder_bucket {
                wanted.push((i, placeholder_bucket));
            }
            page_tiles.insert(
                i + 1,
                render::PageTiles {
                    placement: *placement,
                    under: Vec::new(),
                    tiles: Vec::new(),
                },
            );
        }
        self.pending_visuals = !wanted.is_empty();

        let thumbs = self.rail_thumbnails(&mut thumb_imgs);
        let mut ahead_of_scroll = Vec::new();
        for (i, _) in &prefetch {
            // A page drawn from tiles once it scrolls in needs only the
            // placeholder its tiles arrive over, never a whole bitmap too
            // large for one.
            let bucket = if self.drawn_from_tiles(*i) {
                placeholder_bucket
            } else {
                page_bucket
            };
            if !self.raster_images.contains_key(&(*i, bucket)) {
                // Marked drawn so the bitmap the scroll is about to need is
                // not the one evicted for it.
                let _ = self.raster_images.best(*i, bucket);
                ahead_of_scroll.push((*i, bucket));
            }
        }
        // Order: the screen first, then thumbnails, then what lies ahead.
        let mut order = wanted;
        order.extend(thumbs);
        order.extend(ahead_of_scroll);
        self.submit_page_renders(document_id, &order);
        // Renders still in flight (prefetch included) keep a capture waiting.
        self.pending_visuals |= !self.raster_render_pending.is_empty();

        self.request_tiled_pages(document_id, &wanted_tiles, &mut page_tiles);
        // The worker wakes the window when a result is queued, so no frame
        // spins while it is rasterising.
        (page_imgs, thumb_imgs, page_tiles)
    }

    /// The pages within reach of the canvas, from the page stack's geometry
    /// alone (never a walk over the whole document): those on screen with
    /// their distance from its centre, those `ahead`/`behind` of it with
    /// their distance from its edge, each nearest first, and the on-screen
    /// pages drawn too large for one bitmap, which are drawn from tiles.
    fn pages_in_reach(&self, (ahead, behind): (f64, f64)) -> PagesInReach {
        let canvas = self.canvas_rect;
        let mut tiled = Vec::new();
        let count = self.doc.page_count();
        let unit = self.stack_unit();
        if count == 0 || unit <= 0.0 {
            return PagesInReach {
                visible: Vec::new(),
                prefetch: Vec::new(),
                tiled,
            };
        }
        let pageh = self.ph() * self.zoom();
        let content_y = |y: f64| self.viewport.screen_to_content(Point::new(canvas.x0, y)).y;
        let first = (content_y(canvas.y0 - behind) / unit).floor().max(0.0) as usize;
        let last = ((content_y(canvas.y1 + ahead) / unit).floor().max(0.0) as usize).min(count - 1);
        let origin = |i: usize| {
            self.viewport
                .content_point_to_screen(Point::new(0.0, i as f64 * unit))
        };
        let reach = tilo_page_raster::reach::pages_in_reach(
            first..=last,
            (canvas.y0, canvas.y1),
            (ahead, behind),
            |i| {
                let top = origin(i).y;
                Some((top, top + pageh))
            },
            |i, _| match self.tile_placement(i, origin(i)) {
                Some(placement) => {
                    tiled.push(placement);
                    true
                }
                None => false,
            },
        );
        PagesInReach {
            visible: reach.visible,
            prefetch: reach.prefetch,
            tiled,
        }
    }

    /// The rail thumbnails on screen, drawn from the best bitmap held for
    /// each (`thumb_imgs`, 1-based), and the renders they still want.
    fn rail_thumbnails(
        &mut self,
        thumb_imgs: &mut std::collections::HashMap<usize, PenikoImage>,
    ) -> Vec<(usize, u32)> {
        let mut thumbs = Vec::new();
        if !self.show_rail {
            return thumbs;
        }
        let rail = self.rail_rect;
        // 0-based display indices + widths, from the shared rail's geometry.
        for (i, r) in self.page_sidebar.thumb_rects() {
            if r.y0 > rail.y1 || r.y1 < rail.y0 {
                continue;
            }
            let tw = crate::source::RasterBackend::bucket_for(
                ((r.width() * 2.0).round() as u32).clamp(96, crate::tiles::THUMB_MAX_PX),
            );
            let held = self.raster_images.best(i, tw);
            if let Some((_, image)) = &held {
                thumb_imgs.insert(i + 1, image.clone());
            } else {
                self.pending_visuals = true;
            }
            if held.is_none_or(|(bucket, _)| bucket < tw) {
                thumbs.push((i, tw));
            }
        }
        thumbs
    }

    /// Hand the worker this frame's page renders as one batch replacing its
    /// queue, most important first. A render already asked for keeps its
    /// place; at most [`MAX_NEW_PAGE_RASTERS_PER_FRAME`] new pages and
    /// [`MAX_NEW_THUMB_RASTERS_PER_FRAME`] new thumbnails join per frame,
    /// so the open→first-paint path pays for the top page alone and the rest
    /// follow as results wake later frames.
    fn submit_page_renders(&mut self, document_id: u64, order: &[(usize, u32)]) {
        let batch = tilo_page_raster::admit(
            order,
            &self.raster_render_pending,
            tilo_page_raster::Admission {
                pages: MAX_NEW_PAGE_RASTERS_PER_FRAME,
                thumbnails: MAX_NEW_THUMB_RASTERS_PER_FRAME,
            },
        );
        if batch == self.raster_render_batch {
            return;
        }
        let submitted = self.raster_handle.as_ref().is_some_and(|handle| {
            handle
                .request_renders(crate::source::RenderBatch {
                    document_id,
                    renders: batch
                        .iter()
                        .map(|(page, target_width)| crate::source::RenderRequest {
                            document_id,
                            page: *page,
                            target_width: *target_width,
                        })
                        .collect(),
                })
                .is_ok()
        });
        if submitted {
            self.raster_render_pending = batch.iter().copied().collect();
            self.raster_render_batch = batch;
        } else if !batch.is_empty() {
            self.status_note
                .show(crate::t("pdf-note-pages-unavailable"));
        }
    }

    /// Where a visible page's pixels land if it is drawn from tiles: `None`
    /// when a whole-page bitmap at this zoom stays under
    /// [`crate::tiles::TILE_MIN_PAGE_PX`]. The placement follows the page's
    /// card mapping, so tiles line up with text and markup overlays.
    fn tile_placement(&self, i: usize, origin: Point) -> Option<crate::tiles::PagePlacement> {
        if !self.drawn_from_tiles(i) {
            return None;
        }
        let size = self.display_page_size(i)?;
        let map = self.page_map(i + 1);
        let z = self.zoom();
        let device = map.scale * z * RASTER_SS;
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

    /// Whether 0-based page `i` is drawn from tiles at this zoom: its
    /// whole-page bitmap would pass [`crate::tiles::TILE_MIN_PAGE_PX`].
    fn drawn_from_tiles(&self, i: usize) -> bool {
        let Some(size) = self.display_page_size(i) else {
            return false;
        };
        let width = size.0 * self.page_map(i + 1).scale * self.zoom() * RASTER_SS;
        width > crate::tiles::TILE_MIN_PAGE_PX && width.is_finite()
    }

    /// Displayed size (points, recorded rotation applied) of 0-based page `i`.
    fn display_page_size(&self, i: usize) -> Option<(f64, f64)> {
        self.raster_document.as_ref()?.page_sizes.get(i).copied()
    }

    /// Worker mode for zoomed pages: draw the tiles already here, over the
    /// tiles of other zoom levels still held for the page (scaled, so a zoom
    /// keeps showing the last sharp pixels until the new level's arrive),
    /// and hand the worker the missing tiles as one batch, most important
    /// first. The batch replaces the previous one, so tiles that scrolled
    /// out of view are never rendered ahead of the ones now on screen.
    fn request_tiled_pages(
        &mut self,
        document_id: u64,
        wanted: &[crate::tiles::WantedTile],
        page_tiles: &mut std::collections::HashMap<usize, render::PageTiles>,
    ) {
        let canvas = self.canvas_rect;
        let plan = crate::tiles::plan_tiles(
            wanted,
            &mut self.raster_tiles,
            (canvas.x0, canvas.y0, canvas.x1, canvas.y1),
            page_tiles,
            1,
        );
        self.pending_visuals |= plan.missing;
        let batch: Vec<crate::source::TileRequest> = plan
            .batch
            .iter()
            .map(|key| crate::source::TileRequest {
                document_id,
                page: key.page,
                level: key.level,
                col: key.col,
                row: key.row,
            })
            .collect();
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
                self.status_note
                    .show(crate::t("pdf-note-pages-unavailable"));
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

    /// Ask the PDF worker for the engine text layer of the pages currently in
    /// view; the layers land in `self.raster_text` as they arrive.
    fn refresh_raster_text(&mut self) {
        // The engine text layer feeds the selection and search-hit overlays.
        // It is read on the worker, below the window's thread, and kept
        // (bounded by `evict_text_layers`) once here: a press needs the
        // page's layer, which `hover_text` asks for ahead of it. While a
        // selection, search hits or a highlight drag is live, every page on
        // screen asks for its layer so the overlays follow the scroll.
        let need = self.selection.is_some()
            || !self.raster_matches.is_empty()
            || self
                .drag
                .as_ref()
                .is_some_and(|drag| drag.tool == Tool::Highlight);
        if !need {
            return;
        }
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
                self.status_note.show(crate::t("pdf-note-text-unavailable"));
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
        let waiting;
        let content = if self.pending_open.is_some() {
            EmptyStateContent {
                icon: Some(Icon::FileText),
                ..EmptyStateContent::new("Opening PDF…")
            }
        } else if let Some((title, message, error)) = self.source_wait_text() {
            waiting = message;
            EmptyStateContent {
                icon: Some(if error {
                    Icon::TriangleAlert
                } else {
                    Icon::FileText
                }),
                message: Some(&waiting),
                error,
                ..EmptyStateContent::new(title)
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
                // A window whose record marks up a document keeps it: another
                // file opens in a window of its own, so none is offered here.
                action_label: self.record_source.is_none().then_some("Open another PDF"),
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
        let action = self.file_open.paint(
            &mut PaintCtx::new(scene, base, tr, theme),
            canvas,
            &content,
            self.last_mouse,
            false,
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
                    for r in self.highlight_geometry(d).0 {
                        let scr = Rect::new(
                            origin.x + r.x * self.zoom(),
                            origin.y + r.y * self.zoom(),
                            origin.x + (r.x + r.w) * self.zoom(),
                            origin.y + (r.y + r.h) * self.zoom(),
                        );
                        render::draw_highlight_preview(scene, base, col, scr);
                    }
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

    /// Cover a page-shaped imported occurrence whose verified bytes are not
    /// here yet, so a page still waiting on a collaborator cannot be mistaken
    /// for a blank page of the document.
    fn draw_import_placeholder(
        &self,
        scene: &mut Scene,
        base: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        origin: Point,
        page_index: usize,
    ) {
        let Some(Some(imported)) = self.resource_page_imports.get(page_index) else {
            return;
        };
        let label = match self.content.state(&imported.hash) {
            ContentState::Resolved => return,
            ContentState::Pending => "Imported page is loading…",
            ContentState::Waiting => {
                "Imported page arrives once a collaborator who has it is online"
            }
            ContentState::Unavailable => "Imported page is unavailable",
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
        let (page, rect, size) =
            self.annotations
                .iter()
                .find(|a| a.id == id)
                .and_then(|a| match &a.kind {
                    AnnoKind::FreeText { rect, size, .. } => Some((a.page, *rect, *size)),
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
            // The note is edited in place at its own size on the page, so
            // the text does not jump when the edit ends.
            #[allow(
                clippy::cast_possible_truncation,
                reason = "a note's on-screen type size is far inside f32"
            )]
            input.set_content_size(Some((size * z) as f32));
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

    /// The find highlights on page `page_num`, in its page space: one band
    /// per line a match covers, and whether the match is the current one,
    /// which paints emphasised.
    fn found_bands(&self, page_num: usize) -> Vec<(PtRect, bool)> {
        let Some(layer) = self.raster_text.get(&page_num) else {
            return Vec::new();
        };
        self.raster_matches
            .iter()
            .enumerate()
            .filter(|(_, m)| m.page == page_num)
            .flat_map(|(i, m)| {
                let current = i == self.active_match;
                layer
                    .bands_for_range(m.char_start, m.char_start + m.char_len)
                    .into_iter()
                    .map(move |band| (band, current))
            })
            .collect()
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

        // Find matches, one highlight per line a match covers.
        for (band, current) in self.found_bands(page_num) {
            ui::widget::paint_find_match(scene, base, to_screen(&band), current, theme);
        }

        let Some(layer) = self.raster_text.get(&page_num) else {
            return;
        };

        // Live selection, the part of it on this page.
        if let Some((a, b)) = self
            .selection
            .filter(TextSelection::is_active)
            .and_then(|sel| sel.range_on(page_num, layer.chars.len()))
        {
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
        if sel.dragging || !sel.is_active() || self.editing.is_some() || self.context_menu.is_some()
        {
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
            render::draw_selection_bands(scene, base, theme, &self.selection_screen_bands());
        }
        // Anchor above the top edge of the selection, horizontally centred on it.
        let anchor = Point::new(
            f64::midpoint(bounds_rect.x0, bounds_rect.x1),
            bounds_rect.y0,
        );
        let swatches: Vec<ui::HudSwatch> = Swatch::ALL
            .iter()
            .map(|&s| {
                let (r, g, b) = s.rgb();
                ui::HudSwatch::new([r, g, b], s == self.swatch)
            })
            .collect();
        // The HUD is a floating Panel on the window's theme. It stands under
        // the selection, so a heading above the passage stays readable at
        // any zoom.
        let hud = ui::SelectionHud::draw(
            scene,
            base,
            tr,
            theme,
            // One menu for the selection whatever made it: keyboard
            // shortcuts shown where a keyboard is the way in.
            &ui::HudControls {
                swatches: &swatches,
                show_note: true,
                show_delete: false,
                show_select_all: true,
                shortcuts: self.interaction.modality != ui::interaction::InputModality::Touch,
            },
            // Clear of the end grip's knob, where it shows.
            ui::HudPlacement::new(anchor, self.canvas_rect).below(
                self.selection_grips().map_or(bounds_rect.y1, |grips| {
                    grips.reach_below().max(bounds_rect.y1)
                }),
            ),
            self.last_mouse,
        );

        self.canvas_hover_zones.push(hud.rect());
        self.sel_hud = Some(hud);
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
        let (pw, ph) = (self.pw(), self.ph());

        // The shared page rail draws its region, the thumbnails' frames,
        // the current-page lift, page numbers, markup dots, drag reflow +
        // ghost, scrollbar, context menu and the import affordance. It renders
        // the page content through this closure — a real engine bitmap when
        // the raster backend has one, otherwise the actual vector page. Move the
        // rail out of `self` so the closure can borrow the (immutable)
        // document alongside it.
        let mut sb = std::mem::replace(&mut self.page_sidebar, PageSidebar::new(1.0));
        let doc = &self.doc;
        sb.render(scene, base, tr, theme, |scene, tr, base, i, rect| {
            let num = i + 1;
            if let Some(page) = doc.page(num) {
                render::Thumbnail {
                    page,
                    page_w: pw,
                    page_h: ph,
                    raster: thumb_imgs.get(&num),
                    markup: render::Markup {
                        annotations: &self.annotations,
                        signatures: &self.signatures,
                        selected: None,
                    },
                }
                .draw(&mut PaintCtx::new(scene, base, tr, theme), rect);
            }
        });
        self.page_sidebar = sb;
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
        let labels = tab_labels(self.sidebar_tabs_short());
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
        // The inline editor's rect is routed before any tab hit,
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

    /// Comments are reader chrome, not fixed-size PDF content. The editor
    /// applies accessibility text scale itself; its body size needs UI scale.
    fn comment_body_size(theme: &Theme) -> f32 {
        theme.text(TextRole::Body).size / theme.text_scale
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
        let theme = self.layout_theme.clone();
        let rect = self.rich_text_rect(tr, 0.0, 0.0, width);
        let mut views = self.comment_views.borrow_mut();
        let view = views.entry(key).or_default();
        view.editor_mut()
            .set_body_size(Self::comment_body_size(&theme));
        view.show(blocks);
        view.layout(rect, &theme, tr)
    }

    /// The rect a comment's rich text is laid out in so its glyphs start at
    /// `x` and wrap at `width`: the editor sets each line in from its rect
    /// by its own inset, on both sides.
    fn rich_text_rect(&self, tr: &mut TextRenderer, x: f64, y: f64, width: f64) -> Rect {
        let pad = self.rich_text_inset(tr);
        Rect::new(x - pad, y, x + width + pad, y)
    }

    /// How far in from its rect the rich-text view sets the first glyph of a
    /// line, read from the view itself (where its caret stands before the
    /// first character), so comment text shares the author name's edge
    /// exactly. Measured once per text scale.
    fn rich_text_inset(&self, tr: &mut TextRenderer) -> f64 {
        let theme = &self.layout_theme;
        if let Some(((text_scale, ui_scale), inset)) = self.rich_inset.get()
            && (text_scale - theme.text_scale).abs() < f32::EPSILON
            && (ui_scale - theme.ui_scale).abs() < f32::EPSILON
        {
            return inset;
        }
        let mut view = RichTextView::new();
        view.editor_mut()
            .set_body_size(Self::comment_body_size(theme));
        view.show(&[ui::widget::BlockContent::paragraph("inset", "Inset")]);
        view.layout(Rect::new(0.0, 0.0, 240.0, 0.0), theme, tr);
        let origin = view.rect().x0;
        let inset = view
            .editor_mut()
            .blocks()
            .first()
            .map_or(0.0, |block| block.line.caret_rect(theme, 0).x0 - origin)
            .max(0.0);
        self.rich_inset
            .set(Some(((theme.text_scale, theme.ui_scale), inset)));
        inset
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
            // card. The thread of the markup you have selected is the selected
            // card.
            let face = CardFace::new().pressable().selected(selected).interaction(
                ui::widget::Interaction {
                    hovered: card.contains(self.last_mouse),
                    ..Default::default()
                },
            );
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
        // The body as a field: its box stands one field pad out from the text
        // (the edge the reply field and the Reply button share), so the text
        // itself keeps the author name's edge.
        let pad = f64::from(theme.tokens.spacing.s2);
        let body_hit = Rect::new(
            body_x - pad,
            body_top - tight,
            card.x1,
            body_top + m.body_h + tight,
        );
        let text_rect = self.rich_text_rect(tr, body_x, body_top, text_w);
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
                field.layout(text_rect, theme, tr);
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
            view.editor_mut()
                .set_body_size(Self::comment_body_size(theme));
            view.show(&cm.blocks);
            view.set_text_color(cm.resolved.then_some(theme.text_dim));
            view.layout(text_rect, theme, tr);
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
            let rect = self.rich_text_rect(tr, x0, yy, text_w);
            let mut views = self.comment_views.borrow_mut();
            let view = views
                .entry(comment_view_key(&cm.id, Some(&r.id)))
                .or_default();
            view.editor_mut()
                .set_body_size(Self::comment_body_size(theme));
            view.show(&r.blocks);
            view.set_text_color(cm.resolved.then_some(theme.text_dim));
            view.layout(rect, theme, tr);
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
            let text_rect = self.rich_text_rect(tr, hdr.text_x, field.y0 + pad, text_w - pad);
            if let Some(Editing {
                input: EditInput::Rich(reply),
                ..
            }) = self.editing.as_mut()
            {
                reply.layout(text_rect, theme, tr);
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
        let hint_y = y;
        if self.tool == Tool::Sign {
            let (_, height) = ui::text_render::with_layout_renderer(theme.text_scale, |tr| {
                tr.measure_style(
                    SIGNATURE_PLACE_HINT,
                    Some((x1 - x0).max(1.0) as f32),
                    &theme.text(TextRole::Caption),
                )
            });
            y += f64::from(height) + f64::from(rhythm.related);
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
        let seg_labels = ["Type", "Draw"];
        let seg_rects =
            SegmentedFace::partition(geo.mode, &seg_labels, ControlSize::Regular, tr, theme);
        let sel = usize::from(self.sig_mode == SigMode::Draw);
        ToggleGroup::paint(
            scene,
            base,
            tr,
            theme,
            &SegmentedFace::new(&seg_rects, &seg_labels, sel)
                .hovered(seg_rects.iter().position(|r| r.contains(self.last_mouse))),
        );
        for (mode, rect) in [SigMode::Type, SigMode::Draw].into_iter().zip(&seg_rects) {
            out.hits.push((SidebarHit::SigMode(mode), *rect));
        }

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
            self.signer()
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
                self.signature_ink_rect(),
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
                SIGNATURE_PLACE_HINT,
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

    // ---- the page and the zoom ---------------------------------------

    /// The zoom the bar says, in percent. With no document there is
    /// nothing being zoomed: it reads 100%, not a value fitted to the window.
    pub(crate) fn shown_zoom_percent(&self) -> f64 {
        if self.has_document {
            (self.zoom() * 100.0).round()
        } else {
            100.0
        }
    }

    /// What the window's bar says on a page or zoom change: the page, and
    /// the zoom.
    pub(crate) fn page_line(&self) -> String {
        format!(
            "Page {} of {}, {:.0}%",
            self.current_page,
            self.doc.page_count(),
            self.shown_zoom_percent()
        )
    }

    /// Say the page and the zoom in the window's bar when either changes
    /// while a document is open; opening one is not a change.
    fn follow_page_line(&mut self) {
        if !self.has_document {
            self.page_said = None;
            return;
        }
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a zoom percent is a small positive whole number"
        )]
        let now = (self.current_page, self.shown_zoom_percent() as u32);
        if self.page_said.is_some_and(|said| said != now) {
            self.page_notice.show(self.page_line());
            self.dirty = true;
        }
        self.page_said = Some(now);
    }
}

const SIGNATURE_PLACE_HINT: &str = "Click on a page to place your signature.";

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

/// The pages within reach of the canvas this frame (0-based indices).
struct PagesInReach {
    /// On screen, drawn from one bitmap, with the distance from the canvas
    /// centre; nearest first.
    visible: Vec<(usize, f64)>,
    /// Ahead of or behind the canvas, with the distance from its edge;
    /// nearest first.
    prefetch: Vec<(usize, f64)>,
    /// On screen, drawn from tiles.
    tiled: Vec<crate::tiles::PagePlacement>,
}
