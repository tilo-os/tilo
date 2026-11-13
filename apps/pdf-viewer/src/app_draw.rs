struct CommentMetrics {
    card_h: f64,
    body_h: f64,
    reply_heights: Vec<f64>,
    compose_h: f64,
}

struct CommentCard<'a> {
    rect: Rect,
    comment: &'a Thread,
    metrics: &'a CommentMetrics,
    text_w: f64,
}

/// A comment card's header row on one centre line: avatar, name, Resolve and delete.
/// The name is bounded, so a long one elides instead of running under the buttons.
struct CommentHeader {
    avatar: Rect,
    name: Rect,
    resolve: Rect,
    delete: Rect,
    text_x: f64,
    body_top: f64,
}

struct SidebarHits<'a> {
    hits: &'a mut Vec<(SidebarHit, Rect)>,
    edit_rect: &'a mut Rect,
}

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

fn tab_labels() -> [String; 3] {
    ["pdf-tab-comments", "pdf-tab-contents", "pdf-tab-signing"].map(crate::t)
}

impl PdfApp {
    fn measure_tabs(&mut self, tr: &mut TextRenderer, theme: &Theme) {
        let labels = tab_labels();
        let segments = labels.each_ref().map(Segment::new);
        let rects = SegmentedFace::partition_segments(
            Rect::new(0.0, 0.0, f64::MAX / 4.0, 1.0),
            &segments,
            SegmentedStyle::Tabs,
            ControlSize::Regular,
            tr,
            theme,
        );
        self.tab_widths = Some([0, 1, 2].map(|i| rects.get(i).map_or(0.0, Rect::width)));
    }

    fn sidebar_tab_strip(&self) -> Rect {
        let theme = &self.layout_theme;
        let sb = self.sidebar_panel();
        let h = SegmentedFace::height(ControlSize::Regular, theme);
        let top = sb.y0 + f64::from(theme.tokens.spacing.s2);
        let inset = self.panel_inset();
        Rect::new(sb.x0 + inset, top, sb.x1 - inset, top + h)
    }

    fn sidebar_tab_rects(&self) -> [(SidebarTab, Rect); 3] {
        let strip = self.sidebar_tab_strip();
        let widths = match self.tab_widths {
            Some(widths) => widths,
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

    fn draw_all(&mut self, scene: &mut Scene, base: Affine, tr: &mut TextRenderer, theme: &Theme) {
        self.canvas_hover_zones.clear();
        self.current_page = self.nearest_page();
        self.follow_page_line();
        self.measure_tabs(tr, theme);
        ui::window_toolbar::declare(Some(self.window_toolbar()));

        let chrome_text_scale = tr.text_scale();
        tr.set_text_scale(1.0);
        if self.raster_active() {
            self.refresh_raster_text();
            self.refresh_raster_links();
        }
        tr.set_text_scale(chrome_text_scale);

        self.prepare_sidebar_scroll(tr);

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

        let (page_imgs, thumb_imgs, page_tiles) = self.rasterize_visible();

        if self.has_document {
            tr.set_text_scale(1.0);
        }
        self.draw_canvas(scene, base, tr, theme, &page_imgs, &page_tiles);
        self.draw_field_editor(scene, base, tr, theme);
        let free_text_edit = self.draw_free_text_editor(scene, base, tr, theme);
        tr.set_text_scale(chrome_text_scale);

        self.draw_selection_hud(scene, base, tr, theme);
        if let Some(grips) = self.selection_grips() {
            grips.draw(scene, base, theme);
        }

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

        self.sidebar_hits = hits;
        self.shown.note(self.canvas_shown(base, theme));
    }

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

    /// Where the delete menu for `target` opens: from the pointer's column, above when
    /// there is no room below.
    fn delete_menu_anchor(&self, target: &DeleteTarget, at: Point) -> Rect {
        self.delete_target_screen_rect(target).map_or_else(
            || Rect::new(at.x, at.y, at.x, at.y),
            |r| {
                let x = at.x.clamp(r.x0, r.x1);
                Rect::new(x, r.y0, x, r.y1)
            },
        )
    }

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
            // A page drawn from tiles needs only the placeholder its tiles arrive over.
            let bucket = if self.drawn_from_tiles(*i) {
                placeholder_bucket
            } else {
                page_bucket
            };
            if !self.raster_images.contains_key(&(*i, bucket)) {
                let _ = self.raster_images.best(*i, bucket);
                ahead_of_scroll.push((*i, bucket));
            }
        }
        let mut order = wanted;
        order.extend(thumbs);
        order.extend(ahead_of_scroll);
        self.submit_page_renders(document_id, &order);
        self.pending_visuals |= !self.raster_render_pending.is_empty();

        self.request_tiled_pages(document_id, &wanted_tiles, &mut page_tiles);
        (page_imgs, thumb_imgs, page_tiles)
    }

    /// The pages within reach of the canvas from the stack's geometry alone: those on
    /// screen and those `ahead` or `behind` it, nearest first.
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

    fn rail_thumbnails(
        &mut self,
        thumb_imgs: &mut std::collections::HashMap<usize, PenikoImage>,
    ) -> Vec<(usize, u32)> {
        let mut thumbs = Vec::new();
        if !self.show_rail {
            return thumbs;
        }
        let rail = self.rail_rect;
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

    fn drawn_from_tiles(&self, i: usize) -> bool {
        let Some(size) = self.display_page_size(i) else {
            return false;
        };
        let width = size.0 * self.page_map(i + 1).scale * self.zoom() * RASTER_SS;
        width > crate::tiles::TILE_MIN_PAGE_PX && width.is_finite()
    }

    fn display_page_size(&self, i: usize) -> Option<(f64, f64)> {
        self.raster_document.as_ref()?.page_sizes.get(i).copied()
    }

    /// Draw the tiles already here, over the other levels still held, and hand the
    /// worker the missing ones as one batch, most important first.
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

    /// Ask for the link areas of the pages in view. Links are cheap and must be ready
    /// before the pointer reaches them, so they are fetched on every frame.
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

    fn refresh_raster_text(&mut self) {
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
        let waited;
        let opening = crate::t("pdf-empty-opening");
        let open_failed = crate::t("pdf-empty-open-failed");
        let empty = crate::t("pdf-empty-heading");
        let open = crate::t("pdf-empty-open");
        let open_another = crate::t("pdf-empty-open-another");
        let offers: Vec<ui::widget::DocumentOffer<'_>> = self
            .workspace_documents
            .iter()
            .map(|document| ui::widget::DocumentOffer {
                target: &document.target,
                name: &document.name,
                caption: Some(&document.caption),
            })
            .collect();
        let content = if self.opening() {
            EmptyStateContent {
                icon: Some(Icon::FileText),
                ..EmptyStateContent::new(&opening)
            }
        } else if let Some((title, message, error)) = self.source_wait_text() {
            waited = title;
            waiting = message;
            EmptyStateContent {
                icon: Some(if error {
                    Icon::TriangleAlert
                } else {
                    Icon::FileText
                }),
                message: Some(&waiting),
                error,
                ..EmptyStateContent::new(&waited)
            }
        } else if let Some(failure) = self.open_failure_text() {
            detail = failure;
            EmptyStateContent {
                icon: Some(Icon::TriangleAlert),
                message: Some(&detail),
                action_label: self
                    .record_source
                    .is_none()
                    .then_some(open_another.as_str()),
                error: true,
                ..EmptyStateContent::new(&open_failed)
            }
        } else {
            EmptyStateContent {
                icon: Some(Icon::FileText),
                documents: &offers,
                action_label: Some(&open),
                ..EmptyStateContent::new(&empty)
            }
        };
        let painted = self.file_open.paint(
            &mut PaintCtx::new(scene, base, tr, theme),
            canvas,
            &content,
            self.last_mouse,
            false,
            self.interaction,
        );
        self.empty_open_button
            .set(painted.action.unwrap_or(Rect::ZERO));
        *self.empty_document_rows.borrow_mut() = painted.documents;
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

        if !self.has_document {
            self.draw_empty_state(scene, base, tr, theme, canvas);
            return;
        }

        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, base, &canvas);

        let pageh = self.ph() * self.zoom();
        let origins = self.page_origins();
        for (i, origin) in origins.iter().enumerate() {
            let page_num = i + 1;
            if origin.y > canvas.y1 || origin.y + pageh < canvas.y0 {
                continue;
            }
            let Some(page) = self.doc.page(page_num) else {
                continue;
            };
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
                render::Markup {
                    annotations: &self.annotations,
                    signatures: &self.signatures,
                    selected: self.selected_anno.as_deref(),
                },
            );

            self.draw_import_placeholder(scene, base, tr, theme, *origin, i);
            self.draw_form_fields(scene, base, tr, theme, page_num, *origin);

            self.draw_text_overlays(scene, base, theme, page_num, *origin);
        }

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

        self.viewport.draw_scrollbars(scene, base, theme);
    }

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
            ContentState::Pending => crate::t("pdf-imported-page-loading"),
            ContentState::Waiting => crate::t("pdf-imported-page-waiting"),
            ContentState::Unavailable => crate::t("pdf-imported-page-unavailable"),
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
                ..EmptyStateContent::new(&label)
            },
            Point::new(f64::NEG_INFINITY, f64::NEG_INFINITY),
        );
    }

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
            #[allow(
                clippy::cast_possible_truncation,
                reason = "a note's on-screen type size is far inside f32"
            )]
            input.set_content_size(Some((size * z) as f32));
            draw_inline_editor(input, scene, base, tr, theme, r);
        }
        Some(r)
    }

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

        if let Some(region) = self.region
            && region.page == page_num
        {
            render::draw_region_marquee(scene, base, theme, to_screen(&region.rect()));
        }

        for (band, current) in self.found_bands(page_num) {
            ui::widget::paint_find_match(scene, base, to_screen(&band), current, theme);
        }

        let Some(layer) = self.raster_text.get(&page_num) else {
            return;
        };

        if let Some((a, b)) = self
            .selection
            .filter(TextSelection::is_active)
            .and_then(|sel| sel.range_on(page_num, layer.chars.len()))
        {
            let bands: Vec<Rect> = layer.bands_for_range(a, b).iter().map(to_screen).collect();
            render::draw_selection_bands(scene, base, theme, &bands);
        }
    }

    /// The floating selection HUD over the live selection, rebuilt each frame. Shown once
    /// the drag settled, never while the inline editor is open.
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
        let anchor = Point::new(
            f64::midpoint(bounds_rect.x0, bounds_rect.x1),
            bounds_rect.y0,
        );
        let swatches: Vec<ui::HudSwatch> = HighlightColor::ALL
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
            theme,
            &ui::HudControls {
                swatches: &swatches,
                show_note: true,
                show_delete: false,
                show_select_all: true,
                shortcuts: self.interaction.modality != ui::interaction::InputModality::Touch,
            },
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

        let tab_rects = self.sidebar_tab_rects();
        let rects: Vec<Rect> = tab_rects.iter().map(|(_, r)| *r).collect();
        let labels = tab_labels();
        let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
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
        // Everything pushed from here on is painted inside the `body` clip, so its hit
        // rect is clipped the same way.
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
        let clipped: Vec<(SidebarHit, Rect)> = hits
            .drain(body_hits_start..)
            .filter_map(|(hit, r)| {
                let r = r.intersect(body);
                (r.width() > 0.0 && r.height() > 0.0).then_some((hit, r))
            })
            .collect();
        hits.extend(clipped);
        // Clipped the same way: an editor scrolled under the tab strip must not swallow
        // the press meant for the tab covering it.
        let visible_edit = edit_rect.intersect(body);
        *edit_rect = if visible_edit.width() > 0.0 && visible_edit.height() > 0.0 {
            visible_edit
        } else {
            Rect::ZERO
        };
        hits
    }

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

    fn panel_inset(&self) -> f64 {
        f64::from(self.layout_theme.tokens.spacing.s3)
    }

    fn comments_list_top(&self, body: Rect) -> f64 {
        let theme = &self.layout_theme;
        let rhythm = &theme.tokens.spacing.rhythm;
        let caption = f64::from(theme.text(TextRole::Caption).line_height_px());
        body.y0 + f64::from(rhythm.sibling) + caption + f64::from(rhythm.sibling)
    }

    fn resolve_button_w(tr: &mut TextRenderer, theme: &Theme) -> f64 {
        [
            crate::t("pdf-comment-resolve"),
            crate::t("pdf-comment-reopen"),
        ]
        .iter()
        .map(|label| {
            ButtonFace::new(Rect::ZERO, label)
                .role(ButtonRole::Plain)
                .control_size(ControlSize::Small)
                .intrinsic_width(tr, theme)
        })
        .fold(0.0, f64::max)
    }

    /// The header row's pieces, which never overlap however long the name is.
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
        cm: &Thread,
        card_w: f64,
    ) -> CommentMetrics {
        let rhythm = &theme.tokens.spacing.rhythm;
        let text_w = Self::comment_text_w(theme, card_w);
        let line = f64::from(theme.text(TextRole::Body).line_height_px());
        let label = f64::from(theme.text(TextRole::Label).line_height_px());
        let row_h = ButtonFace::height(ControlSize::Small, theme);
        let bh = match self.editing_field(&EditTarget::CommentBody(cm.id.clone())) {
            Some(field) => field.rect().height().max(line),
            None if cm.comment.text.is_empty() => line,
            None => {
                self.comment_view_height(tr, comment_view_key(&cm.id, None), &cm.blocks(), text_w)
            }
        };
        let mut inner = row_h + f64::from(rhythm.tight) + bh;
        let mut reply_heights = Vec::with_capacity(cm.comment.replies.len());
        for (rid, r) in cm.replies() {
            let rh = if r.text.is_empty() {
                line
            } else {
                self.comment_view_height(
                    tr,
                    comment_view_key(&cm.id, Some(rid)),
                    &reply_blocks(r),
                    text_w,
                )
            };
            reply_heights.push(rh);
            inner += f64::from(rhythm.sibling) + label + f64::from(rhythm.tight) + rh;
        }
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

    fn editing_field(&self, target: &EditTarget) -> Option<&CommentField> {
        match self.editing.as_ref() {
            Some(Editing {
                target: editing,
                input: EditInput::Rich(field),
            }) if editing == target => Some(field),
            _ => None,
        }
    }

    fn comment_body_size(theme: &Theme) -> f32 {
        theme.text(TextRole::Body).size / theme.text_scale
    }

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

    fn rich_text_rect(&self, tr: &mut TextRenderer, x: f64, y: f64, width: f64) -> Rect {
        let pad = self.rich_text_inset(tr);
        Rect::new(x - pad, y, x + width + pad, y)
    }

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

    fn comment_card_w(&self, body: Rect) -> f64 {
        (body.width() - 2.0 * self.panel_inset()).max(1.0)
    }

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

    fn outline_top(&self, body: Rect) -> f64 {
        body.y0 + f64::from(self.layout_theme.tokens.spacing.s2)
    }

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
        let comments = self.comments.clone();

        let total = comments.len();
        let open = comments.iter().filter(|c| !c.comment.resolved).count();
        if total == 0 {
            let title = crate::t("pdf-comments-empty-title");
            let message = crate::t("pdf-comments-empty-message");
            EmptyState::paint(
                &mut PaintCtx::new(scene, base, tr, theme),
                body.inset(-inset),
                &EmptyStateContent {
                    icon: Some(Icon::Pencil),
                    message: Some(&message),
                    ..EmptyStateContent::new(&title)
                },
                self.last_mouse,
            );
            return;
        }
        let summary = comments_summary(total, open);
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

        let mut y = self.comments_list_top(body) - self.sidebar_scroll;
        let resolve_w = Self::resolve_button_w(tr, theme);
        for cm in &comments {
            let selected = self
                .selected_anno
                .as_deref()
                .is_some_and(|sa| cm.pinned() == Some(sa));
            let m = self.comment_metrics(tr, theme, cm, card_w);
            let card = Rect::new(body.x0 + inset, y, body.x0 + inset + card_w, y + m.card_h);
            out.hits.push((SidebarHit::Comment(cm.id.clone()), card));
            if card.y1 < body.y0 || card.y0 > body.y1 {
                y += m.card_h + gap;
                continue;
            }

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

    fn draw_comment_header_row(
        &self,
        ctx: &mut PaintCtx<'_>,
        cm: &Thread,
        hdr: &CommentHeader,
        out: &mut SidebarHits,
    ) {
        let theme = ctx.theme;
        AvatarFace::new(&cm.comment.author, &cm.comment.author).paint(ctx, hdr.avatar);
        let style = theme.text(TextRole::Label).strong();
        let name =
            ctx.text
                .elide_to_width_style(&cm.comment.author, hdr.name.width() as f32, &style);
        let (_, h) = ctx.text.measure_style(&name, None, &style);
        ctx.text(
            &name,
            &style,
            theme.text_primary,
            Point::new(hdr.name.x0, hdr.name.center().y - f64::from(h) / 2.0),
            None,
        );

        let resolve_label = if cm.comment.resolved {
            crate::t("pdf-comment-reopen")
        } else {
            crate::t("pdf-comment-resolve")
        };
        let resolve = ButtonFace::new(hdr.resolve, &resolve_label)
            .role(ButtonRole::Plain)
            .control_size(ControlSize::Small)
            .hovered(hdr.resolve.contains(self.last_mouse));
        Button::paint(ctx.scene, ctx.transform, ctx.text, theme, &resolve);
        out.hits
            .push((SidebarHit::Resolve(cm.id.clone()), hdr.resolve));

        let delete_label = crate::t("pdf-comment-delete");
        let delete = ButtonFace::icon_only(hdr.delete, Icon::X, &delete_label)
            .control_size(ControlSize::Small)
            .hovered(hdr.delete.contains(self.last_mouse));
        Button::paint(ctx.scene, ctx.transform, ctx.text, theme, &delete);
        out.hits.push((
            SidebarHit::DeleteComment(cm.id.clone()),
            delete.hit_rect(theme),
        ));
    }

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
            if let Some(Editing {
                input: EditInput::Rich(field),
                ..
            }) = self.editing.as_mut()
            {
                field.layout(text_rect, theme, tr);
                field.draw(scene, base, tr, theme, &PresenceCtx::empty());
            }
        } else if cm.comment.text.is_empty() {
            let placeholder = crate::t("pdf-viewer-comment-placeholder");
            tr.draw_style(
                scene,
                &placeholder,
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
            view.show(&cm.blocks());
            view.set_text_color(cm.comment.resolved.then_some(theme.text_dim));
            view.layout(text_rect, theme, tr);
            view.draw(scene, base, tr, theme);
        }
        out.hits
            .push((SidebarHit::EditComment(cm.id.clone()), body_hit));
        body_top + m.body_h
    }

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
        for ((rid, r), reply_h) in cm.replies().zip(m.reply_heights.iter()) {
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
                .entry(comment_view_key(&cm.id, Some(rid)))
                .or_default();
            view.editor_mut()
                .set_body_size(Self::comment_body_size(theme));
            view.show(&reply_blocks(r));
            view.set_text_color(cm.comment.resolved.then_some(theme.text_dim));
            view.layout(rect, theme, tr);
            view.draw(scene, base, tr, theme);
            yy += reply_h;
        }
        yy
    }

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
            let label = crate::t("pdf-comment-reply");
            let face = ButtonFace::new(Rect::ZERO, &label)
                .icon(Icon::Reply)
                .role(ButtonRole::Plain)
                .control_size(ControlSize::Small);
            let w = face.intrinsic_width(tr, theme);
            let pad = f64::from(theme.tokens.spacing.s2);
            let rect = Rect::new(
                hdr.text_x - pad,
                top,
                hdr.text_x - pad + w,
                top + ButtonFace::height(ControlSize::Small, theme),
            );
            let face = ButtonFace::new(rect, &label)
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
            let empty = crate::t("pdf-contents-empty");
            EmptyState::paint(
                &mut PaintCtx::new(scene, base, tr, theme),
                body.inset(-self.panel_inset()),
                &EmptyStateContent {
                    icon: Some(Icon::List),
                    ..EmptyStateContent::new(&empty)
                },
                self.last_mouse,
            );
            return;
        }
        let row_h = ui::chrome::nav_row_height(theme);
        let mut y = self.outline_top(body) - self.sidebar_scroll;
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

    fn sig_compose_h(&self) -> f64 {
        let theme = &self.layout_theme;
        let row = ButtonFace::height(ControlSize::Regular, theme);
        match self.sig_mode {
            SigMode::Draw => 3.0 * row,
            SigMode::Type => 2.0 * row,
        }
    }

    fn sig_row_h(&self, tr: &mut TextRenderer) -> f64 {
        ListRowFace::new(&crate::t("pdf-viewer-signing-as"))
            .secondary(&crate::t_args("pdf-a11y-page", &[("page", "1".into())]))
            .height(tr, &self.layout_theme)
    }

    /// The y (before scroll) where the placed-signatures list begins — below
    /// the fixed composer. Kept in one place so `draw_signatures` and
    /// `sidebar_item_layout` never disagree about the composer's height.
    fn sig_list_top(&self, body: Rect) -> f64 {
        self.signature_geometry(body).list_top
    }

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
            let hint = signature_place_hint();
            let (_, height) = ui::text_render::with_layout_renderer(theme, |tr| {
                tr.measure_style(
                    &hint,
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
        let heading_text = crate::t("pdf-viewer-signature-your");
        tr.draw_style(
            scene,
            &heading_text,
            theme.text_primary,
            base * Affine::translate((geo.mode.x0, geo.heading_y)),
            None,
            &heading,
        );

        let seg_labels = [
            crate::t("pdf-signature-type"),
            crate::t("pdf-signature-draw"),
        ];
        let seg_refs = [seg_labels[0].as_str(), seg_labels[1].as_str()];
        let seg_rects =
            SegmentedFace::partition(geo.mode, &seg_refs, ControlSize::Regular, tr, theme);
        let sel = usize::from(self.sig_mode == SigMode::Draw);
        ToggleGroup::paint(
            scene,
            base,
            tr,
            theme,
            &SegmentedFace::new(&seg_rects, &seg_refs, sel)
                .hovered(seg_rects.iter().position(|r| r.contains(self.last_mouse))),
        );
        for (mode, rect) in [SigMode::Type, SigMode::Draw].into_iter().zip(&seg_rects) {
            out.hits.push((SidebarHit::SigMode(mode), *rect));
        }

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
        let signing_as = crate::t("pdf-viewer-signing-as");
        tr.draw_style(
            scene,
            &signing_as,
            theme.text_dim,
            base * Affine::translate((card.x0 + pad, card.y0 + f64::from(theme.tokens.spacing.s2))),
            None,
            &caption,
        );
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
                (crate::t("pdf-signature-your-name"), theme.text_tertiary)
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
            let hint = crate::t("pdf-signature-draw-hint");
            let style = theme.text(TextRole::Body);
            let (hw, hh) = tr.measure_style(&hint, None, &style);
            tr.draw_style(
                scene,
                &hint,
                theme.text_tertiary,
                base * Affine::translate((
                    card.center().x - f64::from(hw) / 2.0,
                    card.center().y - f64::from(hh) / 2.0,
                )),
                None,
                &style,
            );
        } else {
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
        if !self.sig_strokes.is_empty() {
            let clear = crate::t("pdf-signature-clear");
            let probe = ButtonFace::new(Rect::ZERO, &clear)
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
            let face = ButtonFace::new(clr, &clear)
                .role(ButtonRole::Plain)
                .control_size(ControlSize::Small)
                .hovered(clr.contains(self.last_mouse));
            Button::paint(scene, base, tr, theme, &face);
            out.hits.push((SidebarHit::SigClear, clr));
        }
    }

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
            let hint = signature_place_hint();
            tr.draw_style(
                scene,
                &hint,
                theme.text_dim,
                base * Affine::translate((geo.place.x0, geo.hint_y)),
                Some(geo.place.width() as f32),
                &theme.text(TextRole::Caption),
            );
        }
        let label = if arming {
            crate::t("pdf-signature-cancel")
        } else {
            crate::t("pdf-signature-place")
        };
        let face = ButtonFace::new(geo.place, &label)
            .hovered(geo.place.contains(self.last_mouse))
            .disabled(!arming && !ready);
        Button::paint(scene, base, tr, theme, &face);
        out.hits.push((SidebarHit::AddSignature, geo.place));
    }

    fn draw_placed_signatures_list(
        &self,
        ctx: &mut PaintCtx<'_>,
        body: Rect,
        geo: &SignatureGeometry,
        out: &mut SidebarHits,
    ) {
        let theme = ctx.theme;
        let heading = theme.text(TextRole::Subheading);
        let placed = crate::t("pdf-viewer-signature-placed");
        ctx.text(
            &placed,
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
                &crate::t("pdf-viewer-signature-placed-note"),
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
        let delete_label = crate::t("pdf-signature-delete");
        let delete = [RowAction {
            icon: Icon::Trash2,
            label: &delete_label,
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
            let page = crate::t_args("pdf-a11y-page", &[("page", s.page.to_string().into())]);
            let signed = crate::dates::shown(&s.date);
            let hovered = row.contains(self.last_mouse);
            let face = ListRowFace::new(&s.signer)
                .leading(RowLeading::Icon(Icon::PenTool))
                .secondary(&page)
                .meta(&signed)
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

    pub(crate) fn shown_zoom_percent(&self) -> f64 {
        if self.has_document {
            (self.zoom() * 100.0).round()
        } else {
            100.0
        }
    }

    pub(crate) fn page_line(&self) -> String {
        crate::t_args(
            "pdf-page-line",
            &[
                ("page", self.current_page.into()),
                ("pages", self.doc.page_count().into()),
                ("zoom", (self.shown_zoom_percent() as i64).into()),
            ],
        )
    }

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
            self.page_notice.publish(|line| {
                ui::window_toolbar::ToolbarNotice::new(
                    line.clone(),
                    ui::widget::toast::ToastKind::Info,
                )
            });
        }
        self.page_said = Some(now);
    }
}

pub(crate) fn comments_summary(total: usize, open: usize) -> String {
    match (total, open) {
        (1, 1) => crate::t("pdf-comments-one-open"),
        (1, _) => crate::t("pdf-comments-one-resolved"),
        (_, 0) => crate::t_args("pdf-comments-all-resolved", &[("total", total.into())]),
        _ => crate::t_args(
            "pdf-comments-open",
            &[("total", total.into()), ("open", open.into())],
        ),
    }
}

fn signature_place_hint() -> String {
    crate::t("pdf-signature-place-hint")
}

struct SignatureGeometry {
    heading_y: f64,
    mode: Rect,
    compose: Rect,
    hint_y: f64,
    place: Rect,
    placed_y: f64,
    list_top: f64,
}

fn draw_inline_editor(
    input: &mut TextInput,
    scene: &mut Scene,
    base: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
) {
    input.apply_layout_external(rect);
    input.draw_child(scene, base, tr, theme, &PresenceCtx::empty());
}

struct PagesInReach {
    visible: Vec<(usize, f64)>,
    prefetch: Vec<(usize, f64)>,
    tiled: Vec<crate::tiles::PagePlacement>,
}
