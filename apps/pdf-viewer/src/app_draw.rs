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

/// One comment card being painted: its rect, the comment, the metrics it
/// was measured with and the card's inner text width.
struct CommentCard<'a> {
    rect: Rect,
    comment: &'a Comment,
    metrics: &'a CommentMetrics,
    inner_w: f64,
}

/// Non-overlapping layout of a comment/note card's header row. The author name
/// gets a bounded width so a long name (or an email address) truncates instead
/// of running under the Resolve chip and the × button. Pure geometry so both
/// the draw pass and a test can assert the pieces never collide.
struct CommentHeader {
    avatar_c: Point,
    avatar_radius: f64,
    name_x: f64,
    /// Baseline (top) y for the author name.
    name_y: f64,
    /// Max text width for the author name before the resolve chip.
    name_max: f32,
    chip: Rect,
    /// Tight overlay rails use a short visible chip label while keeping the
    /// full resolve action in the semantic tree.
    compact_chip: bool,
    del: Rect,
    /// Top y where the card body should begin, clear of the header row.
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
    /// Toolbar rows this layout occupies: 1, or 2 when the tool cluster no
    /// longer fits between the edge clusters and wraps beneath them.
    rows: usize,
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

/// The find bar's keyboard hint, dropped first when the bar gets narrow.
const SEARCH_HINT: &str = "Enter ▸ next  ·  Esc ▸ close";

/// Non-overlapping layout of the find bar. Computed once as pure geometry from
/// the measured label widths, then shared by the draw pass and the layout test —
/// so the field, counter, toggles and hint provably never collide and reflow
/// gracefully as the window narrows (hint drops first, then the counter
/// compacts to `n/m`, then drops).
struct SearchBarLayout {
    field: Rect,
    /// The match-counter rect + its already-chosen (full or compact) text.
    /// `None` when there is nothing to report or no room for it.
    counter: Option<(Rect, String)>,
    case: Rect,
    whole_word: Rect,
    hint: Option<Rect>,
}

/// Top-left y for a single text run so its *measured* line box is centred in
/// `rect`. `text_h` is the height half of the `TextRenderer::measure*` result
/// for the exact string, size and weight the caller is about to draw — that
/// height already carries the renderer's accessibility text scale. Centring on
/// the nominal font size instead sat every label a quarter line too low at 1×
/// and clipped the Fit / chip labels out of their 34px controls at 2× text.
fn vcenter(rect: Rect, text_h: f32) -> f64 {
    rect.y0 + (rect.height() - f64::from(text_h)) / 2.0
}

/// The type-scale steps as the *nominal* sizes this app hands its renderer.
///
/// Theme tokens arrive already multiplied by the accessibility text scale and
/// `TextRenderer` multiplies again, so the scale is divided out once here
/// (`renderer_input_style`'s rule) and applied exactly once — by whichever
/// renderer scale is in force: the chrome scale for bars, rails and the empty
/// state, 1.0 inside the fixed-layout page pass. Cached from the theme in
/// `build_layout` (next to `text_scale` and `toolbar_inset`) so the pure
/// layout helpers that have no `Theme` at hand — `search_layout`,
/// `comment_metrics` — size text exactly as the draw pass paints it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct TypePx {
    pub xs: f32,
    pub sm: f32,
    pub base: f32,
    pub lg: f32,
}

impl TypePx {
    pub(crate) fn from_theme(theme: &Theme) -> Self {
        let scale = &theme.tokens.typography.scale;
        let s = theme.text_scale;
        Self {
            xs: scale.xs / s,
            sm: scale.sm / s,
            base: scale.base / s,
            lg: scale.lg / s,
        }
    }
}

impl Default for TypePx {
    fn default() -> Self {
        Self::from_theme(&Theme::dark())
    }
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
        // The edge clusters always live on the first row; `tb` may be two
        // rows tall, so centre on the row, not on the whole bar.
        let cy = tb.y0 + TOOLBAR_H / 2.0;
        // Breathing room between the tool cluster and whatever bounds its
        // span (the edge clusters on one row, the bar's insets when wrapped).
        let tools_edge_gap = rhythm::RELATED;
        let w = tb.width();
        let compact = w < 640.0;
        let bw = 34.0;
        let bh = 34.0;
        // One rhythm across the bar (HIG §7b): controls inside a cluster are
        // a related step apart, clusters a sibling step — nothing touches.
        let gap = rhythm::RELATED;
        let cluster_gap = rhythm::SIBLING;
        let row = |x0: f64, width: f64| Rect::new(x0, cy - bh / 2.0, x0 + width, cy + bh / 2.0);

        // ---- right cluster, anchored to the right edge ----
        // Rightmost first, walking left; hidden pieces collapse to ZERO. Both
        // edges use the theme inset cached in `toolbar_inset`.
        let mut rx = tb.x1 - self.toolbar_inset;
        let place = |width: f64, rx: &mut f64| {
            *rx -= width;
            row(*rx, width)
        };
        let toggle_sidebar = place(bw, &mut rx);
        rx -= gap;
        let search = place(bw, &mut rx);
        rx -= cluster_gap;
        let fitw = if compact { bw } else { 40.0 };
        let fit = place(fitw, &mut rx);
        rx -= cluster_gap;
        let zoom_in = place(28.0, &mut rx);
        // The percentage is useful confirmation at normal scale, but its
        // fixed toolbar slot is not a viable control at enlarged text. The
        // fit state remains visible/actionable and the accessible document
        // value still announces the current zoom.
        let zoom_label = if compact || self.text_scale > 1.25 {
            Rect::ZERO
        } else {
            rx -= gap;
            place(46.0, &mut rx)
        };
        rx -= gap;
        let zoom_out = place(28.0, &mut rx);
        let right_start = zoom_out.x0;

        // ---- left cluster: page-rail toggle (LEFT-MOST) + Open + Save ----
        // The compositor owns window chrome (the document name lives in the real
        // titlebar), so the app draws no wordmark/app-name here — Open sits
        // directly after the rail toggle.
        let toggle_rail = row(tb.x0 + self.toolbar_inset, bw);
        let open = row(toggle_rail.x1 + cluster_gap, bw);
        let save = row(open.x1 + gap, bw);
        let left_end = save.x1;

        // ---- centre: tools, centred in the remaining span ----
        // When the span between the edge clusters cannot hold the tool
        // cluster (a 480px window), the tools wrap onto a second toolbar row
        // spanning the full width instead of running under the zoom controls;
        // every action stays reachable and none overlaps another.
        let n = Tool::ALL.len() as f64;
        let tools_w = n * bw + (n - 1.0) * gap;
        let wrap = tools_w + 2.0 * tools_edge_gap > right_start - left_end;
        let (rows, tools_cy, tools_x0, tools_x1) = if wrap {
            (
                2,
                tb.y0 + TOOLBAR_H * 1.5,
                tb.x0 + self.toolbar_inset,
                tb.x1 - self.toolbar_inset,
            )
        } else {
            (1, cy, left_end, right_start)
        };
        let tool_row = |x0: f64, width: f64| {
            Rect::new(x0, tools_cy - bh / 2.0, x0 + width, tools_cy + bh / 2.0)
        };
        let centre = f64::midpoint(tools_x0, tools_x1);
        let mut tx = (centre - tools_w / 2.0).max(tools_x0 + tools_edge_gap);
        let mut tools = Vec::new();
        for t in Tool::ALL {
            tools.push((t, tool_row(tx, bw)));
            tx += bw + gap;
        }
        let tools_end = tx - gap;

        // Swatches sit just right of the tools (on the tools' row), but only
        // when the annotate tools are active and there is genuinely room.
        let annotate = self.tool.annotates();
        let sw = 16.0;
        let sgap = rhythm::RELATED;
        let swatch_span = 5.0 * sw + 4.0 * sgap;
        let sx0 = tools_end + cluster_gap;
        let mut swatches = Vec::new();
        if annotate && sx0 + swatch_span <= tools_x1 - cluster_gap {
            let mut sx = sx0;
            for s in Swatch::ALL {
                swatches.push((
                    s,
                    Rect::new(sx, tools_cy - sw / 2.0, sx + sw, tools_cy + sw / 2.0),
                ));
                sx += sw + sgap;
            }
        }

        ToolbarLayout {
            rows,
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
        if self.tool.annotates() {
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
            if self.show_rail {
                "Hide page thumbnails".to_string()
            } else {
                "Show page thumbnails".to_string()
            },
            lay.toggle_rail,
        ));
        out.push((
            if self.show_sidebar {
                "Hide sidebar  (B)".to_string()
            } else {
                "Show sidebar  (B)".to_string()
            },
            lay.toggle_sidebar,
        ));
        out
    }

    /// The match-counter text, full (`"3 of 12 matches"`) or compact
    /// (`"3/12"`). Empty when there is no query. `"No matches"` / `"0"` when a
    /// query finds nothing.
    fn counter_text(&self, compact: bool) -> String {
        let n = self.match_count();
        if n == 0 {
            return if self.query().is_empty() {
                String::new()
            } else if compact {
                "0".to_string()
            } else {
                "No matches".to_string()
            };
        }
        if compact {
            format!("{}/{}", self.active_match + 1, n)
        } else {
            format!("{} of {} matches", self.active_match + 1, n)
        }
    }

    /// Lay the find bar out so nothing overlaps at any width. The toggles pack
    /// against the right edge; the hint sits just left of them (and is dropped
    /// first as space runs out); the field is left-most and capped, and the
    /// match counter sits just right of the field, compacting then dropping
    /// before it would collide with the hint/toggles.
    fn search_layout(&self, tr: &mut TextRenderer) -> SearchBarLayout {
        const FIELD_MIN: f64 = 150.0;
        const FIELD_MAX: f64 = 520.0;

        let s = self.search_rect;
        let scale = f64::from(tr.text_scale()).clamp(1.0, 2.0);
        let pad = 16.0 * scale;
        let gap = 8.0 * scale;
        let cy = f64::midpoint(s.y0, s.y1);
        let field_top = s.y0 + 8.0 * scale;
        let field_bot = s.y1 - 8.0 * scale;
        let field_left = s.x0 + pad;
        let chip_h = 22.0 * scale;
        let chip = |w: f64, x1: f64| Rect::new(x1 - w, cy - chip_h / 2.0, x1, cy + chip_h / 2.0);

        // Right cluster: [Case] [Whole word], packed against the right edge.
        let case_w = f64::from(tr.measure("Case", self.type_px.sm, None).0) + 22.0 * scale;
        let ww_w = f64::from(tr.measure("Whole word", self.type_px.sm, None).0) + 22.0 * scale;
        let whole_word = chip(ww_w, s.x1 - pad);
        let case = chip(case_w, whole_word.x0 - gap);
        let chips_left = case.x0;

        // Counter (full/compact) + hint measurements.
        let full = self.counter_text(false);
        let short = self.counter_text(true);
        let counter_full_w = if full.is_empty() {
            0.0
        } else {
            f64::from(tr.measure(&full, self.type_px.sm, None).0)
        };
        let counter_short_w = if short.is_empty() {
            0.0
        } else {
            f64::from(tr.measure(&short, self.type_px.sm, None).0)
        };
        let hint_w = f64::from(tr.measure(SEARCH_HINT, self.type_px.xs, None).0);

        // Show the hint only when the field + full counter still clear the
        // toggles with room to spare.
        let block_min = FIELD_MIN * scale
            + if counter_full_w > 0.0 {
                gap + counter_full_w
            } else {
                0.0
            };
        let avail_no_hint = (chips_left - 12.0 * scale) - field_left;
        let show_hint = avail_no_hint - block_min >= hint_w + 26.0 * scale;
        let (hint, right_cluster_left) = if show_hint {
            let hx1 = chips_left - 14.0 * scale;
            let hx0 = hx1 - hint_w;
            (Some(Rect::new(hx0, s.y0, hx1, s.y1)), hx0 - 12.0 * scale)
        } else {
            (None, chips_left - 12.0 * scale)
        };

        // Field (left, capped) + counter (just right of it). Reflow the counter
        // full → compact → dropped as the shared span shrinks.
        let span_right = right_cluster_left;
        let room = (span_right - field_left).max(0.0);
        let (counter_w, counter_str) =
            if counter_full_w > 0.0 && room >= FIELD_MIN * scale + gap + counter_full_w {
                (counter_full_w, Some(full))
            } else if counter_short_w > 0.0 && room >= FIELD_MIN * scale + gap + counter_short_w {
                (counter_short_w, Some(short))
            } else {
                (0.0, None)
            };
        let reserve = if counter_str.is_some() {
            counter_w + gap
        } else {
            0.0
        };
        let field_right = (field_left + FIELD_MAX * scale)
            .min(span_right - reserve)
            .max(field_left + (FIELD_MIN * scale).min(room));
        let field = Rect::new(field_left, field_top, field_right, field_bot);
        let counter = counter_str.map(|t| {
            let cx0 = field.x1 + gap;
            (Rect::new(cx0, s.y0, cx0 + counter_w, s.y1), t)
        });

        SearchBarLayout {
            field,
            counter,
            case,
            whole_word,
            hint,
        }
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
                    self.tooltip =
                        Tooltip::new(label).with_delay(std::time::Duration::from_millis(350));
                    self.tooltip.on_hover_enter(std::time::Duration::ZERO);
                    self.tooltip_anchor = anchor;
                }
            }
            None => {
                if self.tooltip.label() != "" {
                    self.tooltip.on_hover_leave();
                    self.tooltip =
                        Tooltip::new("").with_delay(std::time::Duration::from_millis(350));
                }
            }
        }
        // Animate only while a hover is still counting toward the reveal delay;
        // a shown or absent tooltip needs no further frames.
        self.tooltip_pending = hovering && !self.tooltip.is_visible();
    }

    /// Height of the sidebar's tab strip. One definition for the tab rects,
    /// the hairline under them and the body below — three copies of
    /// `38.0 * text_scale` used to live in three functions.
    fn sidebar_tab_h(&self) -> f64 {
        38.0 * self.text_scale
    }

    /// The y where the comments list starts painting (before scroll): below
    /// the fixed "N threads · M open" header line. Shared by the draw pass,
    /// `sidebar_item_layout` and the reveal-scroll origin so they agree.
    fn comments_list_top(body: Rect, s: f64) -> f64 {
        body.y0 + 12.0 * s + 22.0 * s
    }

    fn sidebar_tab_rects(&self) -> [(SidebarTab, Rect); 3] {
        let sb = self.sidebar_rect;
        let h = self.sidebar_tab_h();
        let w = sb.width() / 3.0;
        [
            (
                SidebarTab::Comments,
                Rect::new(sb.x0, sb.y0, sb.x0 + w, sb.y0 + h),
            ),
            (
                SidebarTab::Outline,
                Rect::new(sb.x0 + w, sb.y0, sb.x0 + 2.0 * w, sb.y0 + h),
            ),
            (
                SidebarTab::Signatures,
                Rect::new(sb.x0 + 2.0 * w, sb.y0, sb.x1, sb.y0 + h),
            ),
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
        // A page change (not the initial open) briefly floats the page bubble.
        if self.has_document
            && self.bubble_shown_page != 0
            && self.current_page != self.bubble_shown_page
        {
            self.page_bubble.poke(ui::time::now());
        }
        self.bubble_shown_page = self.current_page;
        let c = &theme.tokens.colors;

        // Whole-window backdrop.
        scene.fill(Fill::NonZero, base, c.surfaces.base, None, &self.rect);

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
            self.refresh_text_lines(tr, theme);
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
            let cur = self.current_page.saturating_sub(1);
            self.page_sidebar
                .begin_frame(self.rail_rect, self.doc.page_count(), cur);
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

        // On a narrow window the panels float above the canvas behind a scrim.
        if self.overlay_mode && (self.show_rail || self.show_sidebar) {
            // The shared scrim: derived per theme instead of a literal black
            // wash that over-darkens a light desktop.
            ui::chrome::fill_scrim(scene, base, self.canvas_rect, 0.45, theme);
        }

        self.draw_rail(scene, base, tr, theme, &thumb_imgs);
        let mut edit_rect = Rect::ZERO;
        let hits = self.draw_sidebar(scene, base, tr, theme, &mut edit_rect);
        self.edit_rect = edit_rect;
        if let Some(rect) = free_text_edit {
            self.edit_rect = rect;
        }
        self.draw_toolbar(scene, base, tr, theme);
        if self.search_active {
            let shits = self.draw_search(scene, base, tr, theme);
            self.search_hits = shits;
        } else {
            self.search_hits.clear();
        }
        self.draw_page_bubble(scene, base, tr, theme);
        if self.shots_context_menu
            && self.context_menu.is_none()
            && let Some(annotation) = self.annotations.first()
        {
            let target = DeleteTarget::Annotation(annotation.id.clone());
            let at = Point::new(self.canvas_rect.x0 + 76.0, self.canvas_rect.y0 + 190.0);
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

    fn draw_tooltip(&self, scene: &mut Scene, base: Affine, tr: &mut TextRenderer, theme: &Theme) {
        if !self.tooltip.is_visible() || self.tooltip.label().is_empty() {
            return;
        }
        let label = self.tooltip.label().to_string();
        // Render through the shared tooltip chip (opaque overlay surface, soft
        // shadow, measured width, flip/clamp into the window) so the PDF
        // viewer's tooltips match every other app's — no bespoke painter.
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

    /// The no-document surface: a document glyph, a short line of guidance,
    /// and — if the last open failed — the reason.
    #[allow(
        clippy::too_many_lines,
        reason = "the empty state keeps its visual and accessible affordances together"
    )]
    fn draw_empty_state(
        &self,
        scene: &mut Scene,
        base: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        canvas: Rect,
    ) {
        let c = &theme.tokens.colors;
        let cx = f64::midpoint(canvas.x0, canvas.x1);
        let cy = f64::midpoint(canvas.y0, canvas.y1);

        // Everything here is measured at the chrome scale and stacked from
        // tokens, then centred as one group, so the title never runs into
        // the detail line and the label never outgrows its button at 200 %.
        let sp = &theme.tokens.spacing;
        let (title, sub, sub_color) = if self.pending_open.is_some() {
            (
                "Opening PDF…",
                "Reading the selected document.",
                theme.text_dim,
            )
        } else {
            match &self.load_error {
                // A file-open failure is a durable state, not a low-priority hint:
                // use the semantic error ink so it remains legible in both dark
                // and high-contrast palettes.
                Some(err) => {
                    let detail = if err.to_ascii_lowercase().contains("password")
                        || err.to_ascii_lowercase().contains("encrypt")
                    {
                        "This PDF needs a password before it can be opened."
                    } else if err.to_ascii_lowercase().contains("unavailable")
                        || err.to_ascii_lowercase().contains("not a readable pdf")
                    {
                        "The file is unavailable or isn't a readable PDF."
                    } else {
                        "Check the file and try again."
                    };
                    ("Couldn't open this document", detail, c.semantic.danger)
                }
                None => (
                    "No document open",
                    "Choose a PDF to start reading.",
                    theme.text_dim,
                ),
            }
        };
        let title_px = self.type_px.lg;
        let sub_px = self.type_px.sm;
        let label_px = self.type_px.base;
        // Measured with the weight it is painted in: a Semibold run advances
        // wider than the Regular measurement that used to centre it.
        let (tw, th) = tr.measure_weighted(title, title_px, None, TextWeight::Semibold);
        // Error strings can be supplied by a parser, portal or native backend.
        // Keep the visible state to one bounded, centred line instead of
        // handing an arbitrary diagnostic to the text wrapper (which also
        // makes a small failure state resistant to pathological input).  The
        // complete diagnostic remains exposed in the assertive accessibility
        // node and in `load_error` for copy/support workflows.
        let shown_sub = tr.elide_to_width_weighted(
            sub,
            sub_px,
            (canvas.width() - 80.0) as f32,
            TextWeight::Regular,
        );
        let (sw, sh) = tr.measure(&shown_sub, sub_px, None);
        let label = "Open a PDF…";
        let (lw, lh) = tr.measure_weighted(label, label_px, None, TextWeight::Semibold);
        let gsz = 16.0;
        let ggap = f64::from(sp.s2);
        let group_w = gsz + ggap + f64::from(lw);
        // The button is its measured group plus token padding, so it grows
        // with the label instead of clipping it at larger text.
        let btn_w = group_w + 2.0 * f64::from(sp.s6);
        let btn_h = f64::from(lh) + 2.0 * f64::from(sp.s3);

        // Stack: glyph, title, detail, action — gaps from the spacing scale.
        let gw = 78.0;
        let gh = 98.0;
        let gap_title = f64::from(sp.s4);
        let gap_sub = f64::from(sp.s1);
        let gap_btn = f64::from(sp.s1);
        let total = gh + gap_title + f64::from(th) + gap_sub + f64::from(sh) + gap_btn + btn_h;
        let mut y = cy - total / 2.0;

        // Outlined page glyph with a folded corner.
        let g = Rect::new(cx - gw / 2.0, y, cx + gw / 2.0, y + gh);
        y += gh + gap_title;
        // A raised card with its derived edge (HIG §9.5); the ruled lines
        // are the Panel-strength edge so they read on every preset.
        ui::surface::card(
            scene,
            base,
            g,
            f64::from(theme.tokens.radii.md),
            c.surfaces.raised,
        );
        let rule = ui::surface::edge_on_with(c.surfaces.raised, ui::chrome::PANEL_BORDER_ALPHA);
        let fold = {
            let mut p = BezPath::new();
            p.move_to((g.x1 - 22.0, g.y0));
            p.line_to((g.x1, g.y0 + 22.0));
            p.line_to((g.x1 - 22.0, g.y0 + 22.0));
            p.close_path();
            p
        };
        scene.fill(Fill::NonZero, base, c.lift(0.72), None, &fold);
        for i in 0..3 {
            let ly = g.y0 + 44.0 + f64::from(i) * 12.0;
            scene.stroke(
                &Stroke::new(2.0),
                base,
                rule,
                None,
                &Line::new((g.x0 + 16.0, ly), (g.x1 - 16.0, ly)),
            );
        }

        tr.draw_text_weighted(
            scene,
            title,
            title_px,
            theme.text_primary,
            base * Affine::translate((cx - f64::from(tw) / 2.0, y)),
            None,
            TextWeight::Semibold,
        );
        y += f64::from(th) + gap_sub;
        tr.draw_text(
            scene,
            &shown_sub,
            sub_px,
            sub_color,
            base * Affine::translate((cx - f64::from(sw) / 2.0, y)),
            None,
        );
        y += f64::from(sh) + gap_btn;

        let btn = Rect::new(cx - btn_w / 2.0, y, cx + btn_w / 2.0, y + btn_h);
        if self.pending_open.is_some() {
            // Opening is a singular, passive state.  Do not render a second
            // open affordance (or a duplicate toast) while the worker owns the
            // request; the quiet progress rail is enough feedback. Nothing to
            // click, so the cached button rect is emptied.
            self.empty_open_button.set(Rect::ZERO);
            // The rail's track is a lift off the canvas with its derived edge,
            // not a declared border that vanishes on the dark preset.
            let progress = Rect::new(cx - 54.0, btn.y0 + 4.0, cx + 54.0, btn.y0 + 8.0);
            ui::surface::card(scene, base, progress, 2.0, c.lift(1.0));
            scene.fill(
                Fill::NonZero,
                base,
                theme.accent_primary,
                None,
                &RoundedRect::from_rect(
                    Rect::new(
                        progress.x0,
                        progress.y0,
                        progress.x0 + progress.width() * 0.62,
                        progress.y1,
                    ),
                    2.0,
                ),
            );
        } else {
            // Primary action: open a document from within the app. The click
            // handler reads this exact rect back (`empty_open_button_rect`).
            self.empty_open_button.set(btn);
            let hot = btn.contains(self.last_mouse);
            // The shared Primary face (HIG §8): its hover is the toolkit's,
            // not a lift painted over the accent. The folder glyph + label
            // stay a centred group inside it.
            let (bg, _, ink) = ui::widget::face_colors(
                theme,
                ui::widget::ButtonStyle::Primary,
                ui::widget::ButtonInteract {
                    hovered: hot,
                    ..Default::default()
                },
            );
            let shape = RoundedRect::from_rect(btn, f64::from(theme.tokens.radii.md));
            scene.fill(Fill::NonZero, base, bg, None, &shape);
            let gx = btn.x0 + (btn.width() - group_w) / 2.0;
            let gcy = f64::midpoint(btn.y0, btn.y1);
            draw_folder_glyph(
                scene,
                base,
                Rect::new(gx, gcy - gsz / 2.0, gx + gsz, gcy + gsz / 2.0),
                ink,
            );
            tr.draw_text_weighted(
                scene,
                label,
                label_px,
                ink,
                base * Affine::translate((gx + gsz + ggap, vcenter(btn, lh))),
                None,
                TextWeight::Semibold,
            );
        }
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
        scene.fill(
            Fill::NonZero,
            base,
            theme.tokens.colors.surfaces.base,
            None,
            &canvas,
        );

        // No document open: a calm empty surface.
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
            let (matched, active) = self.matched_blocks_for(page_num);
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
                render::SearchBands {
                    matched: &matched,
                    active,
                },
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
                Tool::Ink if d.points.len() >= 2 => {
                    if let Some(&(fx, fy)) = d.points.first() {
                        let mut path = BezPath::new();
                        let p0 =
                            Point::new(origin.x + fx * self.zoom(), origin.y + fy * self.zoom());
                        path.move_to(p0);
                        for p in d.points.get(1..).into_iter().flatten() {
                            path.line_to((
                                origin.x + p.0 * self.zoom(),
                                origin.y + p.1 * self.zoom(),
                            ));
                        }
                        scene.stroke(
                            &Stroke::new((2.4 * self.zoom()).max(1.5)),
                            base,
                            col,
                            None,
                            &path,
                        );
                    }
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
            scene.stroke(
                &Stroke::new(1.0),
                base,
                render::alpha(theme.accent_primary, 140),
                None,
                &r,
            );
            if !self.form_values.contains_key(&w.name) {
                continue;
            }
            // Cover the document's own appearance with the recorded value.
            scene.fill(
                Fill::NonZero,
                base,
                render::paper(theme),
                None,
                &r.inflate(-1.0, -1.0),
            );
            match w.kind {
                tilo_pdf::FieldKind::CheckBox | tilo_pdf::FieldKind::RadioButton => {
                    if self.field_checked(&w) {
                        let inset = r.inflate(-r.width() * 0.22, -r.height() * 0.22);
                        let mut tick = BezPath::new();
                        tick.move_to((inset.x0, inset.center().y));
                        tick.line_to((inset.x0 + inset.width() * 0.4, inset.y1));
                        tick.line_to((inset.x1, inset.y0));
                        scene.stroke(
                            &Stroke::new((r.height() * 0.12).max(1.2)),
                            base,
                            render::ink(),
                            None,
                            &tick,
                        );
                    }
                }
                _ => {
                    let size = (r.height() * 0.6).clamp(6.0, 14.0 * z.max(1.0)) as f32;
                    let text = self.field_text(&w);
                    tr.draw_text(
                        scene,
                        &text,
                        size,
                        render::ink(),
                        base * Affine::translate((
                            r.x0 + 3.0 * z,
                            r.center().y - f64::from(size) * 0.6,
                        )),
                        Some((r.width() - 6.0 * z).max(1.0) as f32),
                    );
                }
            }
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
        scene.fill(Fill::NonZero, base, render::paper(theme), None, &r);
        scene.stroke(&Stroke::new(1.5), base, theme.border_focus, None, &r);
        if let Some(Editing {
            input: EditInput::Plain(input),
            ..
        }) = self.editing.as_mut()
        {
            draw_inline_editor(
                input,
                scene,
                base,
                tr,
                theme,
                Point::new(r.x0 + 2.0 * z, r.y0 + 2.0 * z),
                (
                    (r.width() - 4.0 * z).max(8.0) as f32,
                    (r.height() - 4.0 * z).max(8.0),
                ),
            );
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
        scene.fill(
            Fill::NonZero,
            base,
            theme.tokens.colors.surfaces.base,
            None,
            &r,
        );
        scene.stroke(&Stroke::new(1.5), base, theme.border_focus, None, &r);
        if let Some(Editing {
            input: EditInput::Plain(input),
            ..
        }) = self.editing.as_mut()
        {
            draw_inline_editor(
                input,
                scene,
                base,
                tr,
                theme,
                Point::new(r.x0 + 4.0, r.y0 + 2.0),
                (
                    (r.width() - 8.0).max(8.0) as f32,
                    (r.height() - 4.0).max(8.0),
                ),
            );
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
            tilo_content::ContentState::Pending => "Imported page content is loading…",
            tilo_content::ContentState::Unavailable | tilo_content::ContentState::Resolved => {
                "Imported page content is unavailable"
            }
        };
        let page_rect = Rect::new(
            origin.x,
            origin.y,
            origin.x + self.pw() * self.zoom(),
            origin.y + self.ph() * self.zoom(),
        );
        scene.fill(
            Fill::NonZero,
            base,
            Color::from_rgba8(32, 38, 50, 188),
            None,
            &page_rect,
        );
        tr.draw_text_weighted(
            scene,
            label,
            14.0,
            theme.tokens.colors.text_primary,
            base * Affine::translate((
                page_rect.x0 + 24.0,
                f64::midpoint(page_rect.y0, page_rect.y1),
            )),
            None,
            TextWeight::Medium,
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

    /// Draw the raster text-layer overlays for one page: every search hit as a
    /// translucent band (the active hit emphasised) and the live selection as
    /// an accent band. All rectangles come from the real engine glyph boxes, so
    /// they sit exactly over the printed characters.
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

        // Image-region marquee (Shift + Select): a translucent accent rectangle
        // with a dashed border, drawn even when the page has no text layer.
        if let Some(region) = self.region
            && region.page == page_num
        {
            let sr = to_screen(&region.rect());
            scene.fill(
                Fill::NonZero,
                base,
                render::alpha(theme.accent_primary, 40),
                None,
                &sr,
            );
            scene.stroke(
                &Stroke::new(1.5).with_dashes(0.0, [5.0, 4.0]),
                base,
                theme.accent_primary,
                None,
                &sr,
            );
        }

        let Some(layer) = self.raster_text.get(&page_num) else {
            return;
        };

        // Search hits.
        let hit_col = theme.tokens.colors.semantic.warning;
        for (i, m) in self.raster_matches.iter().enumerate() {
            if m.page != page_num {
                continue;
            }
            let active = i == self.active_match;
            for band in layer.bands_for_range(m.char_start, m.char_start + m.char_len) {
                let sr = to_screen(&band);
                let fill = if active {
                    render::alpha(hit_col, 150)
                } else {
                    render::alpha(hit_col, 70)
                };
                scene.fill(
                    Fill::NonZero,
                    base,
                    fill,
                    None,
                    &RoundedRect::from_rect(sr, 1.5),
                );
                if active {
                    scene.stroke(
                        &Stroke::new(1.25),
                        base,
                        hit_col,
                        None,
                        &RoundedRect::from_rect(sr, 1.5),
                    );
                }
            }
        }

        // Live selection.
        if let Some(sel) = self.selection
            && sel.page == page_num
            && sel.is_active()
        {
            let (a, b) = sel.range();
            for band in layer.bands_for_range(a, b) {
                let sr = to_screen(&band);
                scene.fill(
                    Fill::NonZero,
                    base,
                    render::alpha(theme.accent_primary, 70),
                    None,
                    &sr,
                );
            }
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
                for r in self.selection_page_bands() {
                    let sr = Rect::new(
                        origin.x + r.x * z,
                        origin.y + r.y * z,
                        origin.x + (r.x + r.w) * z,
                        origin.y + (r.y + r.h) * z,
                    );
                    scene.fill(
                        Fill::NonZero,
                        base,
                        render::alpha(theme.accent_primary, 70),
                        None,
                        &sr,
                    );
                }
            }
        }
        // Anchor above the top edge of the selection, horizontally centred on it.
        let anchor = Point::new(
            f64::midpoint(bounds_rect.x0, bounds_rect.x1),
            bounds_rect.y0,
        );
        let c = &theme.tokens.colors;
        // The HUD is a floating Panel: shared ground and derived border.
        let style = ui::ReaderHudStyle {
            surface: ui::chrome::panel_ground(theme),
            border: ui::chrome::panel_border(theme),
            elevation: theme.tokens.elevation.e2,
            text: c.text_primary,
            hover: theme.bg_hover,
            pill: c.surfaces.raised,
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

    /// A transient toast pill (e.g. "Saved to …" or "Copied 42 characters")
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
        let rail = self.rail_rect;
        let markup = self.pages_with_markup();
        let pw = self.pw();

        // The shared page rail draws the panel, thumbnails, selection ring,
        // index numbers, drag reflow + ghost, scrollbar, context menu and the
        // "Import…" affordance. It renders the host's page content through this
        // closure — a real engine bitmap when the raster backend has one,
        // otherwise the abstract block sketch. Move the rail out of `self` so
        // the closure can borrow the (immutable) document alongside it.
        let mut sb = std::mem::replace(&mut self.page_sidebar, PageSidebar::new(1.0));
        let doc = &self.doc;
        sb.render(scene, base, tr, theme, |scene, _tr, base, i, rect| {
            let num = i + 1;
            if let Some(page) = doc.page(num) {
                render::Thumbnail {
                    page,
                    page_w: pw,
                    raster: thumb_imgs.get(&num),
                    is_current: false,
                    has_markup: markup.contains(&num),
                }
                .draw(scene, theme, base, rect);
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
        // The Bar's one hairline, derived against the bar (HIG §9.5).
        chrome::bar_edge(scene, base, c.surfaces.raised, tb.x0, tb.x1, tb.y1);

        let lay = self.toolbar_layout();
        self.draw_toolbar_open_save(scene, base, theme, &lay);
        self.draw_toolbar_tools(scene, base, theme, &lay);
        self.draw_toolbar_zoom_fit(scene, base, tr, theme, &lay);
        self.draw_toolbar_view_toggles(scene, base, theme, &lay);
    }

    /// Open-document and save-annotated-PDF buttons, plus the unsaved-edits dot.
    fn draw_toolbar_open_save(
        &self,
        scene: &mut Scene,
        base: Affine,
        theme: &Theme,
        lay: &ToolbarLayout,
    ) {
        // Open a document (folder button), left-most after the page-rail toggle.
        let can_open = self.pending_open.is_none();
        Self::icon_button_bg(
            scene,
            base,
            theme,
            lay.open,
            false,
            can_open && lay.open.contains(self.last_mouse),
        );
        draw_folder_glyph(
            scene,
            base,
            icon_inset(lay.open),
            if can_open {
                theme.text_primary
            } else {
                theme.text_dim
            },
        );

        // Save annotated PDF (tray/down-arrow). Dimmed until a document is open in
        // the PDF engine, since the export runs there.
        let can_save = self.has_document && self.raster_active();
        Self::icon_button_bg(
            scene,
            base,
            theme,
            lay.save,
            false,
            can_save && lay.save.contains(self.last_mouse),
        );
        let save_col = if can_save {
            theme.text_primary
        } else {
            theme.text_dim
        };
        draw_save_glyph(scene, base, icon_inset(lay.save), save_col);
        // Unsaved page edits (reorder / delete / duplicate / import) surface as
        // an accent dot on the Save button.
        if self.pages_dirty && can_save {
            let d = 3.5;
            let dot = Circle::new((lay.save.x1 - 6.0, lay.save.y0 + 6.0), d);
            scene.fill(Fill::NonZero, base, theme.accent_primary, None, &dot);
        }
    }

    /// The tool-selector row plus (while annotating) the colour swatches.
    fn draw_toolbar_tools(
        &self,
        scene: &mut Scene,
        base: Affine,
        theme: &Theme,
        lay: &ToolbarLayout,
    ) {
        for (t, r) in &lay.tools {
            let active = *t == self.tool;
            let hovered = r.contains(self.last_mouse);
            Self::icon_button_bg(scene, base, theme, *r, active, hovered);
            let col = if active || hovered {
                theme.text_primary
            } else {
                theme.text_dim
            };
            draw_tool_icon(scene, base, *t, *r, col);
        }

        let annotate = self.tool.annotates();
        if annotate {
            for (s, r) in &lay.swatches {
                let col = render::swatch_color(theme, *s);
                let center = Point::new(f64::midpoint(r.x0, r.x1), f64::midpoint(r.y0, r.y1));
                let active = *s == self.swatch;
                let rad = if active { 8.0 } else { 6.5 };
                scene.fill(Fill::NonZero, base, col, None, &Circle::new(center, rad));
                if active {
                    scene.stroke(
                        &Stroke::new(2.0),
                        base,
                        theme.text_primary,
                        None,
                        &Circle::new(center, rad + 2.5),
                    );
                }
            }
        }
    }

    /// Zoom out/in + the zoom percentage label, plus the Fit button.
    fn draw_toolbar_zoom_fit(
        &self,
        scene: &mut Scene,
        base: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        lay: &ToolbarLayout,
    ) {
        Self::icon_button_bg(
            scene,
            base,
            theme,
            lay.zoom_out,
            false,
            lay.zoom_out.contains(self.last_mouse),
        );
        draw_minus(scene, base, lay.zoom_out, theme.text_primary);
        Self::icon_button_bg(
            scene,
            base,
            theme,
            lay.zoom_in,
            false,
            lay.zoom_in.contains(self.last_mouse),
        );
        draw_plus(scene, base, lay.zoom_in, theme.text_primary);
        if lay.zoom_label.area() > 1.0 {
            let zlabel = format!("{}%", (self.zoom() * 100.0).round() as i64);
            let (zw, zh) = tr.measure(&zlabel, self.type_px.sm, None);
            tr.draw_text(
                scene,
                &zlabel,
                self.type_px.sm,
                theme.text_primary,
                base * Affine::translate((
                    f64::midpoint(lay.zoom_label.x0, lay.zoom_label.x1) - f64::from(zw) / 2.0,
                    vcenter(lay.zoom_label, zh),
                )),
                None,
            );
        }

        let fit_active = !matches!(self.fit, FitMode::Custom(_));
        Self::icon_button_bg(
            scene,
            base,
            theme,
            lay.fit,
            fit_active,
            lay.fit.contains(self.last_mouse),
        );
        let fit_txt = match self.fit {
            FitMode::Width => "Wide",
            _ => "Fit",
        };
        let (fw, fh) = tr.measure(fit_txt, self.type_px.sm, None);
        tr.draw_text(
            scene,
            fit_txt,
            self.type_px.sm,
            if fit_active {
                theme.text_primary
            } else {
                theme.text_dim
            },
            base * Affine::translate((
                f64::midpoint(lay.fit.x0, lay.fit.x1) - f64::from(fw) / 2.0,
                vcenter(lay.fit, fh),
            )),
            None,
        );
    }

    /// Search, page-rail and sidebar toggle buttons.
    fn draw_toolbar_view_toggles(
        &self,
        scene: &mut Scene,
        base: Affine,
        theme: &Theme,
        lay: &ToolbarLayout,
    ) {
        Self::icon_button_bg(
            scene,
            base,
            theme,
            lay.search,
            self.search_active,
            lay.search.contains(self.last_mouse),
        );
        draw_search_glyph(
            scene,
            base,
            lay.search,
            if self.search_active {
                theme.text_primary
            } else {
                theme.text_dim
            },
        );
        Self::icon_button_bg(
            scene,
            base,
            theme,
            lay.toggle_rail,
            self.show_rail,
            lay.toggle_rail.contains(self.last_mouse),
        );
        draw_rail_glyph(
            scene,
            base,
            lay.toggle_rail,
            if self.show_rail {
                theme.text_primary
            } else {
                theme.text_dim
            },
        );
        Self::icon_button_bg(
            scene,
            base,
            theme,
            lay.toggle_sidebar,
            self.show_sidebar,
            lay.toggle_sidebar.contains(self.last_mouse),
        );
        draw_panel_glyph(
            scene,
            base,
            lay.toggle_sidebar,
            if self.show_sidebar {
                theme.text_primary
            } else {
                theme.text_dim
            },
        );
    }

    fn icon_button_bg(
        scene: &mut Scene,
        base: Affine,
        theme: &Theme,
        r: Rect,
        active: bool,
        hovered: bool,
    ) {
        // The chosen tool / open panel is a lift with the derived hairline and
        // glow (HIG §9.3), never an accent wash: the accent stays free to mean
        // live state on this bar. The face backs off the hit rect by half an
        // `s1` so two neighbouring faces never touch.
        let inset = rhythm::TIGHT / 2.0;
        chrome::row(
            scene,
            base,
            theme,
            r.inflate(-inset, -inset),
            f64::from(theme.tokens.radii.md),
            SelectionState::new(active, hovered, false),
        );
    }

    // ---- search strip ----------------------------------------------

    fn draw_search(
        &mut self,
        scene: &mut Scene,
        base: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) -> Vec<(SearchHit, Rect)> {
        let s = self.search_rect;
        let glass = theme.material_glass();
        scene.fill(Fill::NonZero, base, glass, None, &s);
        chrome::bar_edge(scene, base, glass, s.x0, s.x1, s.y1);

        let lay = self.search_layout(tr);
        self.draw_search_field(scene, base, tr, theme, &lay);
        self.draw_search_counter(scene, base, tr, theme, &lay);
        let hits = self.draw_search_toggles(scene, base, tr, theme, &lay);
        self.draw_search_hint(scene, base, tr, theme, &lay);
        hits
    }

    /// The PDF-specific field shell and magnifier around the shared editor.
    fn draw_search_field(
        &mut self,
        scene: &mut Scene,
        base: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        lay: &SearchBarLayout,
    ) {
        let c = &theme.tokens.colors;
        let field = lay.field;
        let scale = f64::from(theme.text_scale).clamp(1.0, 2.0);
        // The field rests with its derived edge and wears the focus ring only
        // while the shared editor actually has focus (HIG §9.5).
        let radius = f64::from(theme.tokens.radii.md);
        ui::surface::card(scene, base, field, radius, c.surfaces.base);
        if ui::widget::Widget::is_focused(&self.search_input) {
            scene.stroke(
                &Stroke::new(1.5),
                base,
                theme.border_focus,
                None,
                &RoundedRect::from_rect(field, radius),
            );
        }
        draw_search_glyph(
            scene,
            base,
            Rect::new(field.x0, field.y0, field.x0 + 30.0 * scale, field.y1),
            theme.text_dim,
        );

        let input = Rect::new(
            field.x0 + 32.0 * scale,
            field.y0,
            field.x1 - 5.0 * scale,
            field.y1,
        );
        self.search_input.apply_layout_external(input);
        if self.search_input.value().is_empty() {
            let placeholder = "Search document…";
            let (_, ph) = tr.measure(placeholder, self.type_px.base, None);
            tr.draw_text(
                scene,
                placeholder,
                self.type_px.base,
                theme.text_dim,
                base * Affine::translate((input.x0, vcenter(input, ph))),
                Some(input.width() as f32),
            );
        }
        self.search_input
            .draw(scene, base, tr, theme, &PresenceCtx::empty());
    }

    /// Match counter, left-aligned just right of the field (already chosen
    /// full/compact and sized to fit by the layout).
    fn draw_search_counter(
        &self,
        scene: &mut Scene,
        base: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        lay: &SearchBarLayout,
    ) {
        if let Some((rect, text)) = &lay.counter {
            let (_, counter_h) = tr.measure(text, self.type_px.sm, None);
            tr.draw_text(
                scene,
                text,
                self.type_px.sm,
                theme.text_dim,
                base * Affine::translate((rect.x0, vcenter(self.search_rect, counter_h))),
                None,
            );
        }
    }

    /// Find-bar option toggles: Case + Whole word. Each is a pill the user
    /// clicks to flip; the matcher honours both live. Returns their hit rects.
    fn draw_search_toggles(
        &self,
        scene: &mut Scene,
        base: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        lay: &SearchBarLayout,
    ) -> Vec<(SearchHit, Rect)> {
        let mut hits: Vec<(SearchHit, Rect)> = Vec::new();
        for (label, active, hit, r) in [
            (
                "Case",
                self.search_case_sensitive,
                SearchHit::ToggleCaseSensitive,
                lay.case,
            ),
            (
                "Whole word",
                self.search_whole_word,
                SearchHit::ToggleWholeWord,
                lay.whole_word,
            ),
        ] {
            // Chip quiet at rest (raised, derived edge); the active option is
            // the accent fill with no edge (HIG §9.5).
            let radius = r.height() / 2.0;
            let txt = if active {
                scene.fill(
                    Fill::NonZero,
                    base,
                    theme.accent_primary,
                    None,
                    &RoundedRect::from_rect(r, radius),
                );
                theme.tokens.colors.on_accent(4.5)
            } else {
                chrome::quiet_pill(scene, base, theme, r, radius);
                theme.text_dim
            };
            let (lw, lh) = tr.measure(label, self.type_px.sm, None);
            tr.draw_text(
                scene,
                label,
                self.type_px.sm,
                txt,
                base * Affine::translate((
                    f64::midpoint(r.x0, r.x1) - f64::from(lw) / 2.0,
                    vcenter(r, lh),
                )),
                None,
            );
            hits.push((hit, r));
        }
        hits
    }

    fn draw_search_hint(
        &self,
        scene: &mut Scene,
        base: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        lay: &SearchBarLayout,
    ) {
        if let Some(hint) = lay.hint {
            let (_, hint_h) = tr.measure(SEARCH_HINT, self.type_px.xs, None);
            tr.draw_text(
                scene,
                SEARCH_HINT,
                self.type_px.xs,
                theme.text_dim,
                base * Affine::translate((hint.x0, vcenter(self.search_rect, hint_h))),
                None,
            );
        }
    }

    // ---- sidebar ----------------------------------------------------

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
        let sb = self.sidebar_rect;
        let c = &theme.tokens.colors;
        scene.fill(Fill::NonZero, base, c.surfaces.raised, None, &sb);
        // The panel's divider derives from the panel it edges (HIG §9.5).
        chrome::divider(scene, base, c.surfaces.raised, sb.x0, sb.y0, sb.y1);

        // Tabs — shared underline segmented control (accent underline under
        // the active tab; label lifts to the primary colour + semibold).
        let tab_rects = self.sidebar_tab_rects();
        let rects: Vec<Rect> = tab_rects.iter().map(|(_, r)| *r).collect();
        let labels: Vec<&str> = tab_rects
            .iter()
            .map(|(tab, _)| {
                if self.text_scale > 1.25 {
                    match tab {
                        SidebarTab::Comments => "Notes",
                        SidebarTab::Outline => "TOC",
                        SidebarTab::Signatures => "Sign",
                    }
                } else {
                    match tab {
                        SidebarTab::Comments => "Comments",
                        SidebarTab::Outline => "Contents",
                        SidebarTab::Signatures => "Signatures",
                    }
                }
            })
            .collect();
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
                .style(SegmentedStyle::Underline)
                .text_size(self.type_px.sm)
                .weight(TextWeight::Regular),
        );
        for (tab, r) in tab_rects {
            hits.push((SidebarHit::Tab(tab), r));
        }
        let tab_h = self.sidebar_tab_h();
        chrome::bar_edge(scene, base, c.surfaces.raised, sb.x0, sb.x1, sb.y0 + tab_h);

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
        // Hit rect == painted rect. Cards, chips and rows scrolled under the
        // tab strip (or off the bottom) were still pushed at their unclipped
        // positions; because `apply_sidebar` picks the smallest containing
        // rect, an invisible 24px delete button scrolled under "Contents" beat
        // the tab and deleted a comment. Clip every body hit to the body and
        // drop the ones nothing of which remains visible.
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

    /// The scrollable body rect of the sidebar (below the tab strip).
    fn sidebar_body(&self) -> Rect {
        let sb = self.sidebar_rect;
        Rect::new(sb.x0, sb.y0 + self.sidebar_tab_h(), sb.x1, sb.y1)
    }

    /// Measured heights for one comment card. Computed once and shared by the
    /// scroll layout and the draw pass so the two never disagree about how tall
    /// a card is (which would misplace every card below it).
    /// Lay out a comment/note card header so the avatar, author name, resolve
    /// chip and delete button never overlap regardless of name length. The name
    /// is clamped to the space left of the chip (with an 8px gap), and the body
    /// starts below the header band.
    fn comment_header_layout(card: Rect, text_scale: f64) -> CommentHeader {
        // TextRenderer scales glyph metrics, but this hand-drawn header owns
        // its own controls. Scale the whole header band with type so the
        // author, resolve state and delete affordance remain separate at 2×.
        let s = text_scale.clamp(1.0, 2.0);
        let edge = 10.0 * s;
        let del_side = Self::comment_control_side(s);
        let del = Rect::new(
            card.x1 - edge - del_side,
            card.y0 + 6.0 * s,
            card.x1 - edge,
            card.y0 + 6.0 * s + del_side,
        );
        let avatar_radius = 9.0 * s;
        let avatar_c = Point::new(
            card.x0 + 12.0 * s + avatar_radius,
            card.y0 + 6.0 * s + avatar_radius,
        );
        let name_x = avatar_c.x + 5.0 * s + avatar_radius;
        let chip_right = del.x0 - 8.0 * s;
        // Keep an author column even in the narrow overlay. The chip gives up
        // width first; its paint pass then uses the short Fix/Done label.
        let chip_w = (54.0 * s).min((chip_right - (name_x + 24.0 + 16.0 * s)).max(36.0));
        let chip = Rect::new(
            chip_right - chip_w,
            card.y0 + 6.0 * s,
            chip_right,
            card.y0 + 6.0 * s + 16.0 * s,
        );
        // Stop 8px short of the chip so a long name truncates cleanly.
        let name_max = ((chip.x0 - 8.0 * s) - name_x).max(0.0) as f32;
        CommentHeader {
            avatar_c,
            avatar_radius,
            name_x,
            name_y: card.y0 + 8.0 * s,
            name_max,
            chip,
            compact_chip: chip_w < 54.0 * s - 0.5,
            del,
            body_top: card.y0 + Self::comment_body_top(s),
        }
    }

    /// Side of the header's delete control: scales with type, never under a
    /// real pointer target.
    fn comment_control_side(s: f64) -> f64 {
        (18.0 * s).max(24.0)
    }

    /// Where a comment card's body starts, below its header band: the band's
    /// top inset, the control's height, and two tight steps so the body's
    /// focus ring (which reaches a tight step above the text) clears the
    /// delete control instead of overlapping it by 4px at 1×. Shared by the
    /// header layout and `comment_metrics` so the card is measured the way it
    /// is painted.
    fn comment_body_top(s: f64) -> f64 {
        6.0 * s + Self::comment_control_side(s) + 2.0 * rhythm::TIGHT * s
    }

    fn comment_metrics(&self, tr: &mut TextRenderer, cm: &Comment, inner_w: f64) -> CommentMetrics {
        let s = f64::from(tr.text_scale()).clamp(1.0, 2.0);
        let line = f64::from(tr.measure("M", self.type_px.sm, None).1);
        // The body is the live editor while this comment is focused, else its
        // text as written (a line's height when empty, for the hint).
        let body_wrap = f64::from(Self::comment_body_wrap(inner_w, s));
        let bh = match self.editing_field(&EditTarget::CommentBody(cm.id.clone())) {
            Some(field) => field.rect().height().max(line),
            None if cm.body.is_empty() => line,
            None => {
                self.comment_view_height(tr, comment_view_key(&cm.id, None), &cm.blocks, body_wrap)
            }
        };
        // Header band, then the body, then a 12px bottom pad. The band's
        // height is `comment_body_top`, the same value the header layout
        // paints with.
        let mut card_h = Self::comment_body_top(s) + bh + 12.0 * s;
        let mut reply_heights = Vec::with_capacity(cm.replies.len());
        let reply_wrap = f64::from(Self::comment_reply_wrap(inner_w, s));
        for r in &cm.replies {
            let rh = if r.body.is_empty() {
                line
            } else {
                self.comment_view_height(
                    tr,
                    comment_view_key(&cm.id, Some(&r.id)),
                    &r.blocks,
                    reply_wrap,
                )
            };
            reply_heights.push(rh);
            card_h += 16.0 * s + rh + 8.0 * s;
        }
        // Reply-compose row: a slim "Reply" affordance, or the open field.
        let compose_h = match self.editing_field(&EditTarget::Reply(cm.id.clone())) {
            Some(field) => 8.0 * s + field.rect().height().max(line).max(15.0 * s) + 16.0 * s,
            None => 8.0 * s + 24.0 * s,
        };
        card_h += compose_h;
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

    /// A read-only view of a comment or reply, in comment type.
    fn comment_view(&self) -> RichTextView {
        let mut view = RichTextView::new();
        view.editor_mut().set_body_size(self.type_px.sm);
        view
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
        let theme = Theme::dark().with_text_scale(tr.text_scale());
        let mut views = self.comment_views.borrow_mut();
        let view = views.entry(key).or_insert_with(|| self.comment_view());
        view.show(blocks);
        view.layout(Rect::new(0.0, 0.0, width, 0.0), &theme, tr)
    }

    /// Wrap width of a comment card's body text. The body is painted from
    /// `card.x0 + 12·s`, so wrapping to `inner_w − 24·s` keeps the same 12·s
    /// inset on the right; the old unscaled `− 16` left 4px on the right at 1×
    /// and ran 8px past the card border at 2× text. One function feeds both
    /// the measured card height and the paint pass.
    fn comment_body_wrap(inner_w: f64, s: f64) -> f32 {
        (inner_w - 24.0 * s) as f32
    }

    /// Wrap width of a reply body, painted from `card.x0 + 24·s` with the same
    /// 12·s right inset as the body above it.
    fn comment_reply_wrap(inner_w: f64, s: f64) -> f32 {
        (inner_w - 36.0 * s) as f32
    }

    /// Wrap width of the reply composer's text: the field is inset 12·s from
    /// the card and the shared editor insets its text a further 10·s.
    fn comment_compose_wrap(inner_w: f64, s: f64) -> f32 {
        (inner_w - 44.0 * s) as f32
    }

    /// Content-relative layout of the active sidebar list as
    /// `(id, top, height)` entries plus the total content height. Tops are
    /// measured from the same origin the draw routines use *before* the
    /// scroll offset is applied, so the reveal math lines up exactly.
    fn sidebar_item_layout(&self, tr: &mut TextRenderer) -> (Vec<(String, f64, f64)>, f64) {
        let body = self.sidebar_body();
        let s = f64::from(tr.text_scale()).clamp(1.0, 2.0);
        let pad = 14.0 * s;
        let inner_w = body.width() - 2.0 * pad;
        let mut items = Vec::new();
        match self.sidebar_tab {
            SidebarTab::Comments => {
                let list_origin = Self::comments_list_top(body, s);
                let mut y = list_origin;
                for cm in &self.comments {
                    let card_h = self.comment_metrics(tr, cm, inner_w).card_h;
                    items.push((cm.id.clone(), y, card_h));
                    y += card_h + 12.0 * s;
                }
                (items, y - list_origin)
            }
            SidebarTab::Signatures => {
                // The composer (header + Type/Draw toggle + compose card + place
                // button + PLACED header) is fixed; only the placed rows scroll.
                let base = self.sig_list_top(body);
                let mut y = base;
                for s in &self.signatures {
                    items.push((s.id.clone(), y, SIG_ROW_H));
                    y += SIG_ROW_H + rhythm::SIBLING;
                }
                (items, y - base)
            }
            SidebarTab::Outline => {
                // Contents is a genuine navigable list, not a static preview.
                // Keep its geometry here with the other sidebar lists so wheel
                // scrolling and draw-time hit regions remain in lockstep.
                let row_h = 30.0 * s;
                let row_gap = 2.0 * s;
                let mut y = body.y0 + 38.0 * s;
                for (index, _) in self.doc.outline.iter().enumerate() {
                    items.push((format!("outline:{index}"), y, row_h));
                    y += row_h + row_gap;
                }
                (items, (y - (body.y0 + 38.0 * s)).max(0.0))
            }
        }
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
        let s = f64::from(tr.text_scale()).clamp(1.0, 2.0);
        let list_origin = match self.sidebar_tab {
            SidebarTab::Comments => body.y0 + 34.0 * s,
            SidebarTab::Signatures => self.sig_list_top(body) + 20.0,
            SidebarTab::Outline => body.y0 + 38.0 * s,
        };
        let viewport = (body.y1 - list_origin).max(0.0);
        let (items, content_h) = self.sidebar_item_layout(tr);
        let max_scroll = (content_h - viewport).max(0.0);

        if let Some(id) = self.sidebar_reveal.take()
            && let Some((_, top, h)) = items.iter().find(|(iid, _, _)| iid == &id)
        {
            // `top` is content-relative-from-list-origin; bring it just
            // inside the viewport. Both origins come from the same helpers the
            // draw pass and `sidebar_item_layout` use, so a reveal lands on the
            // card rather than 66–104px past it (the old additive chain here
            // predated the Type/Draw composer and never tracked it).
            let list_origin = match self.sidebar_tab {
                SidebarTab::Comments => Self::comments_list_top(body, s),
                SidebarTab::Signatures => self.sig_list_top(body),
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
        let s = f64::from(theme.text_scale).clamp(1.0, 2.0);
        let pad = 14.0 * s;
        let inner_w = body.width() - 2.0 * pad;
        let header_y = body.y0 + 12.0 * s;
        // Scrollable region starts below the fixed header line.
        let mut y = Self::comments_list_top(body, s) - self.sidebar_scroll;
        // Snapshot the comment cache so the loop can borrow `self.editing`
        // mutably (to draw the inline editor) without aliasing `self.comments`.
        let comments = self.comments.clone();

        let total = self.comments.len();
        let open = self.comments.iter().filter(|c| !c.resolved).count();
        let header = if total == 0 {
            "No comments yet".to_string()
        } else {
            format!("{total} threads · {open} open")
        };
        tr.draw_text(
            scene,
            &header,
            self.type_px.sm,
            theme.text_dim,
            base * Affine::translate((body.x0 + pad, header_y)),
            None,
        );

        for cm in &comments {
            let selected = self
                .selected_anno
                .as_deref()
                .is_some_and(|sa| cm.anno_id.as_deref() == Some(sa));
            let m = self.comment_metrics(tr, cm, inner_w);
            let card_h = m.card_h;
            let card = Rect::new(body.x0 + pad, y, body.x1 - pad, y + card_h);

            // Cull cards scrolled out of the body, but keep advancing `y`.
            if card.y1 < body.y0 || card.y0 > body.y1 {
                out.hits.push((SidebarHit::Comment(cm.id.clone()), card));
                y += card_h + 12.0 * s;
                continue;
            }

            // The card is one tier above the raised sidebar with its derived
            // edge; the selected thread is the chosen one — the same face under
            // the stronger edge and glow (HIG §9.3), not an accent wash.
            let c = &theme.tokens.colors;
            chrome::card(
                scene,
                base,
                card,
                f64::from(theme.tokens.radii.md),
                c.surfaces.overlay,
                selected,
            );

            // Header row: avatar + author name (bounded so it never runs under
            // the chips) + resolve chip + delete button, laid out so nothing
            // overlaps regardless of the name's length.
            let hdr = Self::comment_header_layout(card, s);
            self.draw_comment_header_row(scene, base, tr, theme, cm, &hdr, out);
            let comment_card = CommentCard {
                rect: card,
                comment: cm,
                metrics: &m,
                inner_w,
            };
            let ctx = &mut PaintCtx::new(scene, base, tr, theme);
            let yy = self.draw_comment_body(ctx, &comment_card, &hdr, out);
            let yy = self.draw_comment_replies(ctx, &comment_card, yy);
            self.draw_comment_composer(ctx, &comment_card, yy, out);

            out.hits.push((SidebarHit::Comment(cm.id.clone()), card));
            y += card_h + 12.0 * s;
        }
    }

    /// A comment card's header row: avatar + author name (bounded so it never
    /// runs under the chips) + delete button + resolve chip, laid out so
    /// nothing overlaps regardless of the name's length.
    fn draw_comment_header_row(
        &self,
        scene: &mut Scene,
        base: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        cm: &Comment,
        hdr: &CommentHeader,
        out: &mut SidebarHits,
    ) {
        let c = &theme.tokens.colors;
        draw_avatar(
            scene,
            base,
            tr,
            theme,
            &cm.author,
            hdr.avatar_c,
            hdr.avatar_radius,
        );
        let name = tr.elide_to_width_weighted(
            &cm.author,
            self.type_px.sm,
            hdr.name_max,
            TextWeight::Semibold,
        );
        tr.draw_text_weighted(
            scene,
            &name,
            self.type_px.sm,
            theme.text_primary,
            base * Affine::translate((hdr.name_x, hdr.name_y)),
            Some(hdr.name_max),
            TextWeight::Semibold,
        );
        // Right-edge chips: a delete (×) button, then the resolve chip.
        let del = hdr.del;
        let del_hot = del.contains(self.last_mouse);
        if del_hot {
            scene.fill(
                Fill::NonZero,
                base,
                render::alpha(c.semantic.danger, 40),
                None,
                &RoundedRect::from_rect(del, f64::from(theme.tokens.radii.sm)),
            );
        }
        let dc = Point::new(f64::midpoint(del.x0, del.x1), f64::midpoint(del.y0, del.y1));
        let dcol = if del_hot {
            c.semantic.danger
        } else {
            theme.text_dim
        };
        let s = f64::from(theme.text_scale).clamp(1.0, 2.0);
        scene.stroke(
            &Stroke::new(1.5 * s),
            base,
            dcol,
            None,
            &Line::new(
                (dc.x - 4.0 * s, dc.y - 4.0 * s),
                (dc.x + 4.0 * s, dc.y + 4.0 * s),
            ),
        );
        scene.stroke(
            &Stroke::new(1.5 * s),
            base,
            dcol,
            None,
            &Line::new(
                (dc.x - 4.0 * s, dc.y + 4.0 * s),
                (dc.x + 4.0 * s, dc.y - 4.0 * s),
            ),
        );
        out.hits.push((
            SidebarHit::DeleteComment(cm.id.clone()),
            chrome::pointer_target(del, theme),
        ));

        let chip = hdr.chip;
        let (chip_label, chip_fill, chip_text) = if cm.resolved {
            (
                if hdr.compact_chip { "Done" } else { "Resolved" },
                c.semantic.success,
                theme.on_surface(c.semantic.success).ink,
            )
        } else {
            (
                if hdr.compact_chip { "Fix" } else { "Resolve" },
                c.surfaces.raised,
                theme.text_dim,
            )
        };
        // An open thread's chip is quiet: a raised face with its derived edge
        // on the overlay card; a resolved one carries the success tone.
        ui::surface::card(scene, base, chip, chip.height() / 2.0, chip_fill);
        let (cw2, ch2) = tr.measure(chip_label, self.type_px.xs, None);
        tr.draw_text(
            scene,
            chip_label,
            self.type_px.xs,
            chip_text,
            base * Affine::translate((
                f64::midpoint(chip.x0, chip.x1) - f64::from(cw2) / 2.0,
                vcenter(chip, ch2),
            )),
            None,
        );
        out.hits.push((SidebarHit::Resolve(cm.id.clone()), chip));
    }

    /// A comment card's body — the static text, or the shared inline editor
    /// while it's focused (click to edit; caret while focused). Returns the
    /// running `y` where the replies list should start.
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
            inner_w,
        } = *comment_card;
        let s = f64::from(theme.text_scale).clamp(1.0, 2.0);
        let body_x = card.x0 + 12.0 * s;
        let body_top = hdr.body_top;
        let body_wrap = Self::comment_body_wrap(inner_w, s);
        let editing_body = self.is_editing(&EditTarget::CommentBody(cm.id.clone()));
        let body_hit = Rect::new(
            body_x - 4.0 * s,
            body_top - 4.0 * s,
            card.x1 - 10.0 * s,
            body_top + m.body_h + 6.0 * s,
        );
        if editing_body {
            scene.stroke(
                &Stroke::new(1.5),
                base,
                theme.border_focus,
                None,
                &RoundedRect::from_rect(body_hit, f64::from(theme.tokens.radii.sm)),
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
                    Rect::new(body_x, body_top, body_x + f64::from(body_wrap), body_top),
                    theme,
                    tr,
                );
                field.draw(scene, base, tr, theme, &PresenceCtx::empty());
            }
        } else if cm.body.is_empty() {
            tr.draw_text(
                scene,
                "Write a comment…",
                self.type_px.sm,
                theme.text_dim,
                base * Affine::translate((body_x, body_top)),
                Some(body_wrap),
            );
        } else {
            let mut views = self.comment_views.borrow_mut();
            let view = views
                .entry(comment_view_key(&cm.id, None))
                .or_insert_with(|| self.comment_view());
            view.show(&cm.blocks);
            view.layout(
                Rect::new(body_x, body_top, body_x + f64::from(body_wrap), body_top),
                theme,
                tr,
            );
            view.draw(scene, base, tr, theme);
        }
        out.hits
            .push((SidebarHit::EditComment(cm.id.clone()), body_hit));
        body_top + m.body_h + 10.0 * s
    }

    /// A comment card's reply thread — one entry per stored reply, each
    /// preceded by a left rail. Returns the updated running `y`.
    fn draw_comment_replies(
        &self,
        ctx: &mut PaintCtx<'_>,
        comment_card: &CommentCard<'_>,
        mut yy: f64,
    ) -> f64 {
        let (scene, base, tr, theme) = (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
        let CommentCard {
            rect: card,
            comment: cm,
            metrics: m,
            inner_w,
        } = *comment_card;
        let c = &theme.tokens.colors;
        let s = f64::from(theme.text_scale).clamp(1.0, 2.0);
        for (r, reply_h) in cm.replies.iter().zip(m.reply_heights.iter()) {
            let rail = Rect::new(
                card.x0 + 14.0 * s,
                yy - 2.0 * s,
                card.x0 + 15.5 * s,
                yy + reply_h + 10.0 * s,
            );
            // The thread rail derives from the card it sits on, at the
            // Panel-border strength so a 1.5px line still registers.
            scene.fill(
                Fill::NonZero,
                base,
                ui::surface::edge_on_with(c.surfaces.overlay, ui::chrome::PANEL_BORDER_ALPHA),
                None,
                &rail,
            );
            tr.draw_text_weighted(
                scene,
                &r.author,
                10.0,
                theme.text_dim,
                base * Affine::translate((card.x0 + 24.0 * s, yy)),
                None,
                TextWeight::Semibold,
            );
            yy += 14.0 * s;
            let x0 = card.x0 + 24.0 * s;
            let mut views = self.comment_views.borrow_mut();
            let view = views
                .entry(comment_view_key(&cm.id, Some(&r.id)))
                .or_insert_with(|| self.comment_view());
            view.show(&r.blocks);
            view.layout(
                Rect::new(
                    x0,
                    yy,
                    x0 + f64::from(Self::comment_reply_wrap(inner_w, s)),
                    yy,
                ),
                theme,
                tr,
            );
            view.draw(scene, base, tr, theme);
            yy += reply_h + 10.0 * s;
        }
        yy
    }

    /// A comment card's reply composer: the live editor field while composing
    /// a reply, or the "+ Reply" affordance that starts one.
    fn draw_comment_composer(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        comment_card: &CommentCard<'_>,
        yy: f64,
        out: &mut SidebarHits,
    ) {
        let (scene, base, tr, theme) = (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
        let CommentCard {
            rect: card,
            comment: cm,
            metrics: m,
            ..
        } = *comment_card;
        let c = &theme.tokens.colors;
        let composing = self.is_editing(&EditTarget::Reply(cm.id.clone()));
        let s = f64::from(theme.text_scale).clamp(1.0, 2.0);
        let compose_top = yy + 8.0 * s;
        if composing {
            // `comment_metrics` builds `compose_h` as `8·s + field + 16·s`;
            // subtracting an unscaled 8 here made the field run 8px past its
            // measured card at 2× text (the layout test already asserted the
            // scaled form).
            let field_h = m.compose_h - 8.0 * s;
            let field = Rect::new(
                card.x0 + 12.0 * s,
                compose_top,
                card.x1 - 12.0 * s,
                compose_top + field_h,
            );
            scene.fill(
                Fill::NonZero,
                base,
                c.surfaces.raised,
                None,
                &RoundedRect::from_rect(field, f64::from(theme.tokens.radii.sm)),
            );
            scene.stroke(
                &Stroke::new(1.5),
                base,
                theme.border_focus,
                None,
                &RoundedRect::from_rect(field, f64::from(theme.tokens.radii.sm)),
            );
            *out.edit_rect = field;
            let rwrap = Self::comment_compose_wrap(card.width(), s);
            let tx = field.x0 + 10.0 * s;
            let ty = field.y0 + 8.0 * s;
            // The SDK editor draws the reply text, caret and selection.
            if let Some(Editing {
                input: EditInput::Rich(reply),
                ..
            }) = self.editing.as_mut()
            {
                reply.layout(Rect::new(tx, ty, tx + f64::from(rwrap), ty), theme, tr);
                reply.draw(scene, base, tr, theme, &PresenceCtx::empty());
            }
            out.hits
                .push((SidebarHit::ReplyField(cm.id.clone()), field));
        } else {
            let btn = Rect::new(
                card.x0 + 12.0 * s,
                compose_top,
                card.x1 - 12.0 * s,
                compose_top + 22.0 * s,
            );
            let hot = btn.contains(self.last_mouse);
            chrome::row(
                scene,
                base,
                theme,
                btn,
                f64::from(theme.tokens.radii.sm),
                SelectionState::new(false, hot, false),
            );
            let (_, reply_h) = tr.measure("+ Reply", self.type_px.sm, None);
            tr.draw_text(
                scene,
                "+ Reply",
                self.type_px.sm,
                if hot {
                    theme.text_primary
                } else {
                    theme.text_dim
                },
                base * Affine::translate((btn.x0 + 6.0 * s, vcenter(btn, reply_h))),
                None,
            );
            out.hits.push((SidebarHit::ReplyField(cm.id.clone()), btn));
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
        let s = f64::from(theme.text_scale).clamp(1.0, 2.0);
        let pad = 12.0 * s;
        let row_h = 30.0 * s;
        let row_gap = 2.0 * s;
        let header_y = body.y0 + 12.0 * s;
        let header = if self.doc.outline.is_empty() {
            "No contents available".to_string()
        } else {
            format!("{} sections", self.doc.outline.len())
        };
        tr.draw_text(
            scene,
            &header,
            11.0,
            theme.text_dim,
            base * Affine::translate((body.x0 + pad, header_y)),
            None,
        );

        let mut y = body.y0 + 38.0 * s - self.sidebar_scroll;
        for item in &self.doc.outline {
            let row = Rect::new(body.x0 + 6.0 * s, y, body.x1 - 6.0 * s, y + row_h);
            if row.y1 < body.y0 || row.y0 > body.y1 {
                y += row_h + row_gap;
                continue;
            }
            let current = item.page == self.current_page;
            // The rows sit on the raised sidebar, where the shared tier's
            // `lift(1.0)` coincides with the panel; the current page is the
            // same chosen face the comment cards use (one tier up, stronger
            // edge and glow), hover is the tier's lift.
            if current {
                chrome::card(
                    scene,
                    base,
                    row,
                    f64::from(theme.tokens.radii.md),
                    theme.tokens.colors.surfaces.overlay,
                    true,
                );
            } else {
                chrome::row(
                    scene,
                    base,
                    theme,
                    row,
                    f64::from(theme.tokens.radii.md),
                    SelectionState::new(false, row.contains(self.last_mouse), false),
                );
            }
            let indent = pad + f64::from(item.level) * 16.0 * s;
            let weight = if item.level == 0 {
                TextWeight::Semibold
            } else {
                TextWeight::Regular
            };
            let col = if current || item.level == 0 {
                theme.text_primary
            } else {
                theme.text_dim
            };
            let (_, title_h) = tr.measure_weighted(&item.title, self.type_px.sm, None, weight);
            tr.draw_text_weighted(
                scene,
                &item.title,
                self.type_px.sm,
                col,
                base * Affine::translate((row.x0 + indent, vcenter(row, title_h))),
                Some((row.width() - indent - 40.0 * s) as f32),
                weight,
            );
            let pnum = format!("{}", item.page);
            let (pw, ph) = tr.measure(&pnum, self.type_px.sm, None);
            tr.draw_text(
                scene,
                &pnum,
                self.type_px.sm,
                theme.text_dim,
                base * Affine::translate((row.x1 - 10.0 * s - f64::from(pw), vcenter(row, ph))),
                None,
            );
            hits.push((SidebarHit::Outline(item.page), row));
            y += row_h + row_gap;
        }
    }

    /// Height of the signature compose card, which differs by mode (the draw
    /// pad is taller than the name field). Shared by the draw pass and the
    /// scroll layout so they agree on where the placed list begins.
    fn sig_compose_h(&self) -> f64 {
        match self.sig_mode {
            SigMode::Draw => 96.0,
            SigMode::Type => 58.0,
        }
    }

    /// The y (before scroll) where the placed-signatures list begins — below the
    /// fixed composer. Kept in one place so `draw_signatures` and
    /// `sidebar_item_layout` never disagree about the composer's height.
    fn sig_list_top(&self, body: Rect) -> f64 {
        // header(18) + toggle(34)+sibling gap + compose+section gap +
        // place(38)+section gap + PLACED header(20): the same steps
        // `draw_signatures` lays the column out with.
        body.y0
            + 12.0
            + 18.0
            + (34.0 + rhythm::SIBLING)
            + (self.sig_compose_h() + rhythm::SECTION)
            + (38.0 + rhythm::SECTION)
            + 20.0
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
        let pad = 14.0;
        let mut y = body.y0 + 12.0;

        tr.draw_text(
            scene,
            "YOUR SIGNATURE",
            self.type_px.xs,
            theme.text_dim,
            base * Affine::translate((body.x0 + pad, y)),
            None,
        );
        y += 18.0;

        // Type / Draw mode toggle (shared segmented control).
        let tgl = Rect::new(body.x0 + pad, y, body.x1 - pad, y + 34.0);
        let half = tgl.width() / 2.0;
        let seg_rects = [
            Rect::new(tgl.x0, tgl.y0, tgl.x0 + half, tgl.y1),
            Rect::new(tgl.x0 + half, tgl.y0, tgl.x1, tgl.y1),
        ];
        let seg_labels = ["Type", "Draw"];
        let sel = usize::from(self.sig_mode == SigMode::Draw);
        ToggleGroup::paint(
            scene,
            base,
            tr,
            theme,
            &SegmentedFace::new(&seg_rects, &seg_labels, sel)
                .style(SegmentedStyle::Solid)
                .text_size(self.type_px.sm)
                .weight(TextWeight::Medium),
        );
        out.hits
            .push((SidebarHit::SigMode(SigMode::Type), seg_rects[0]));
        out.hits
            .push((SidebarHit::SigMode(SigMode::Draw), seg_rects[1]));
        y += 34.0 + rhythm::SIBLING;

        // Compose card — a name field (Type) or an interactive draw pad (Draw).
        let card = Rect::new(body.x0 + pad, y, body.x1 - pad, y + self.sig_compose_h());
        self.draw_signature_compose_card(scene, base, tr, theme, card, out);
        y += self.sig_compose_h() + rhythm::SECTION;

        let ctx = &mut PaintCtx::new(scene, base, tr, theme);
        y = self.draw_signature_place_button(ctx, body, pad, y, out);

        self.draw_placed_signatures_list(ctx, body, pad, y, out);
    }

    /// The compose card: a name field (Type mode) or an interactive draw pad
    /// (Draw mode) — whichever supplies the signature ink that gets stamped.
    fn draw_signature_compose_card(
        &mut self,
        scene: &mut Scene,
        base: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        card: Rect,
        out: &mut SidebarHits,
    ) {
        self.sig_pad_rect = Rect::ZERO;
        match self.sig_mode {
            SigMode::Type => self.draw_signature_compose_type(scene, base, tr, theme, card, out),
            SigMode::Draw => self.draw_signature_compose_draw(scene, base, tr, theme, card, out),
        }
    }

    /// The signer-name field: editable inline when it's the active edit
    /// target, else the placeholder or the stored name. Returns the shown
    /// name text (used by the caller's live script preview).
    fn draw_signature_name_field(
        &mut self,
        scene: &mut Scene,
        base: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        card: Rect,
        out: &mut SidebarHits,
    ) -> String {
        let editing_signer = self.is_editing(&EditTarget::Signer);
        let name_x = card.x0 + 12.0;
        let name_y = card.y0 + 22.0;
        let name_hit = Rect::new(
            card.x0 + 8.0,
            card.y0 + 19.0,
            card.x1 - 100.0,
            card.y1 - 6.0,
        );
        let name_shown: String = if editing_signer {
            self.editing.as_ref().map(Editing::text).unwrap_or_default()
        } else {
            self.signer_name.clone()
        };
        if editing_signer {
            scene.stroke(
                &Stroke::new(1.5),
                base,
                theme.border_focus,
                None,
                &RoundedRect::from_rect(name_hit, f64::from(theme.tokens.radii.sm)),
            );
            *out.edit_rect = name_hit;
            if let Some(Editing {
                input: EditInput::Plain(input),
                ..
            }) = self.editing.as_mut()
            {
                draw_inline_editor(
                    input,
                    scene,
                    base,
                    tr,
                    theme,
                    Point::new(name_x, name_y),
                    ((name_hit.width() - 6.0) as f32, 18.0),
                );
            }
        } else if name_shown.is_empty() {
            tr.draw_text(
                scene,
                "Your name",
                self.type_px.base,
                theme.text_dim,
                base * Affine::translate((name_x, name_y)),
                None,
            );
        } else {
            tr.draw_text_weighted(
                scene,
                &name_shown,
                self.type_px.base,
                theme.text_primary,
                base * Affine::translate((name_x, name_y)),
                Some((name_hit.width() - 6.0) as f32),
                TextWeight::Semibold,
            );
        }
        out.hits.push((SidebarHit::EditSigner, name_hit));
        name_shown
    }

    /// The "type your name" composer: signer-name field (editable inline)
    /// plus a live script preview of the stamped signature.
    fn draw_signature_compose_type(
        &mut self,
        scene: &mut Scene,
        base: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        card: Rect,
        out: &mut SidebarHits,
    ) {
        let c = &theme.tokens.colors;
        // A field: the base fill with its derived edge (HIG §9.5).
        ui::surface::card(
            scene,
            base,
            card,
            f64::from(theme.tokens.radii.md),
            c.surfaces.base,
        );
        tr.draw_text(
            scene,
            "SIGNING AS",
            9.0,
            theme.text_dim,
            base * Affine::translate((card.x0 + 12.0, card.y0 + 9.0)),
            None,
        );
        let name_shown = self.draw_signature_name_field(scene, base, tr, theme, card, out);
        // Live preview: the typed name rendered as the script that will
        // be stamped.
        let prev = Rect::new(
            card.x1 - 96.0,
            card.y0 + 8.0,
            card.x1 - 12.0,
            card.y1 - 10.0,
        );
        render::draw_signature_preview(
            scene,
            tr,
            base,
            render::ink(),
            prev,
            &SignatureInk::Typed,
            &name_shown,
        );
    }

    /// The freehand draw pad: the user drags here to draw their signature,
    /// with a clear link once there is something drawn.
    fn draw_signature_compose_draw(
        &mut self,
        scene: &mut Scene,
        base: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        card: Rect,
        out: &mut SidebarHits,
    ) {
        let c = &theme.tokens.colors;
        // The draw pad: a field (base fill, derived edge) that wears the
        // focus ring while a stroke is in progress.
        let radius = f64::from(theme.tokens.radii.md);
        ui::surface::card(scene, base, card, radius, c.surfaces.base);
        if self.sig_stroke.is_some() {
            scene.stroke(
                &Stroke::new(1.5),
                base,
                theme.border_focus,
                None,
                &RoundedRect::from_rect(card, radius),
            );
        }
        self.sig_pad_rect = card;
        let ink_area = card.inflate(-10.0, -10.0);
        if self.sig_strokes.is_empty() && self.sig_stroke.is_none() {
            let hint = "Draw your signature here";
            let (hw, hh) = tr.measure(hint, self.type_px.sm, None);
            tr.draw_text(
                scene,
                hint,
                self.type_px.sm,
                theme.text_dim,
                base * Affine::translate((
                    f64::midpoint(card.x0, card.x1) - f64::from(hw) / 2.0,
                    vcenter(card, hh),
                )),
                None,
            );
        } else {
            // Draw the committed strokes plus the in-progress one.
            let mut strokes = self.sig_strokes.clone();
            if let Some(cur) = &self.sig_stroke {
                strokes.push(cur.clone());
            }
            render::draw_signature_preview(
                scene,
                tr,
                base,
                theme.text_primary,
                ink_area,
                &SignatureInk::Drawn { strokes },
                "",
            );
        }
        // Clear link (top-right), only when there is something to clear.
        if !self.sig_strokes.is_empty() {
            let (cw, ch) = tr.measure("Clear", self.type_px.xs, None);
            // The link sits a related step inside the pad's corner, clear of
            // the rounded edge.
            let clr = Rect::new(
                card.x1 - rhythm::RELATED - f64::from(cw) - 2.0 * rhythm::RELATED,
                card.y0 + rhythm::RELATED,
                card.x1 - rhythm::RELATED,
                card.y0 + rhythm::RELATED + 18.0,
            );
            let hot = clr.contains(self.last_mouse);
            tr.draw_text(
                scene,
                "Clear",
                self.type_px.xs,
                if hot {
                    theme.accent_primary
                } else {
                    theme.text_dim
                },
                base * Affine::translate((clr.x0 + rhythm::RELATED, vcenter(clr, ch))),
                None,
            );
            out.hits.push((SidebarHit::SigClear, clr));
        }
    }

    /// The "Place signature" button — arms the Sign tool so the next canvas
    /// click stamps the composed ink. Returns the updated running `y`.
    fn draw_signature_place_button(
        &self,
        ctx: &mut PaintCtx<'_>,
        body: Rect,
        pad: f64,
        y: f64,
        out: &mut SidebarHits,
    ) -> f64 {
        let (scene, base, tr, theme) = (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
        let add = Rect::new(body.x0 + pad, y, body.x1 - pad, y + 38.0);
        let arming = self.tool == Tool::Sign;
        let ready = self.composed_ink().is_some();
        let label = if arming {
            "Click a page to place signature"
        } else if !ready {
            "Draw a signature to place"
        } else {
            "Place signature on a page"
        };
        // The shared button face (HIG §8): the Neutral rest face with its
        // derived edge, or the Primary face while the tool is armed — a live
        // state the accent is for (HIG §9.4).
        let face = ui::widget::ButtonFace::new(add, label)
            .text_size(self.type_px.sm)
            .weight(TextWeight::Semibold)
            .hovered(add.contains(self.last_mouse));
        let face = if arming { face.primary() } else { face };
        ui::widget::Button::paint(scene, base, tr, theme, &face);
        out.hits.push((SidebarHit::AddSignature, add));
        y + 38.0 + rhythm::SECTION
    }

    /// The "PLACED" header + the scrollable list of already-placed
    /// signatures on the document.
    fn draw_placed_signatures_list(
        &self,
        ctx: &mut PaintCtx<'_>,
        body: Rect,
        pad: f64,
        y_start: f64,
        out: &mut SidebarHits,
    ) {
        let (scene, base, tr, theme) = (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
        tr.draw_text(
            scene,
            &format!("PLACED  ·  {}", self.signatures.len()),
            self.type_px.xs,
            theme.text_dim,
            base * Affine::translate((body.x0 + pad, y_start)),
            None,
        );
        // The placed list scrolls; the composer above stays fixed.
        let mut y = y_start + 20.0 - self.sidebar_scroll;

        for (i, s) in self.signatures.iter().enumerate() {
            let row = Rect::new(body.x0 + pad, y, body.x1 - pad, y + SIG_ROW_H);
            if row.y1 >= body.y0 && row.y0 <= body.y1 {
                self.draw_placed_signature_row(scene, base, tr, theme, row, s, out);
            }
            out.hits.push((SidebarHit::SigJump(i), row));
            y += SIG_ROW_H + rhythm::SIBLING;
        }
    }

    /// One row of the placed-signatures list: preview thumbnail, signer +
    /// page/date, and the delete (×) button.
    fn draw_placed_signature_row(
        &self,
        scene: &mut Scene,
        base: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        row: Rect,
        s: &Signature,
        out: &mut SidebarHits,
    ) {
        let c = &theme.tokens.colors;
        let selected = self.selected_sig.as_deref() == Some(s.id.as_str());
        // One tier up from the sidebar with its derived edge; the selected
        // signature is the chosen one — stronger edge and glow, no accent wash.
        chrome::card(
            scene,
            base,
            row,
            f64::from(theme.tokens.radii.md),
            c.surfaces.overlay,
            selected,
        );
        // Delete (×) on the right: painted 24 px, hit at the pointer metric, a
        // sibling step in from the row's edge and centred on the row.
        let control = 24.0;
        let del = Rect::new(
            row.x1 - rhythm::SIBLING - control,
            row.center().y - control / 2.0,
            row.x1 - rhythm::SIBLING,
            row.center().y + control / 2.0,
        );
        let sg = Rect::new(row.x0 + 8.0, row.y0 + 8.0, row.x0 + 64.0, row.y1 - 8.0);
        render::draw_signature_preview(scene, tr, base, theme.text_primary, sg, &s.ink, &s.signer);
        // Signer and meta stop a related step short of the delete control so a
        // long name truncates instead of running under it.
        let text_x = sg.x1 + rhythm::SIBLING;
        let text_max = (del.x0 - rhythm::RELATED - text_x).max(0.0) as f32;
        tr.draw_text_weighted(
            scene,
            &s.signer,
            self.type_px.sm,
            theme.text_primary,
            base * Affine::translate((text_x, row.y0 + 9.0)),
            Some(text_max),
            TextWeight::Semibold,
        );
        tr.draw_text(
            scene,
            &format!("Page {} · {}", s.page, s.date),
            self.type_px.xs,
            theme.text_dim,
            base * Affine::translate((text_x, row.y0 + 26.0)),
            Some(text_max),
        );
        let del_hot = del.contains(self.last_mouse);
        if del_hot {
            scene.fill(
                Fill::NonZero,
                base,
                render::alpha(c.semantic.danger, 40),
                None,
                &RoundedRect::from_rect(del, f64::from(theme.tokens.radii.sm)),
            );
        }
        let dc = Point::new(f64::midpoint(del.x0, del.x1), f64::midpoint(del.y0, del.y1));
        let dcol = if del_hot {
            c.semantic.danger
        } else {
            theme.text_dim
        };
        scene.stroke(
            &Stroke::new(1.5),
            base,
            dcol,
            None,
            &Line::new((dc.x - 4.0, dc.y - 4.0), (dc.x + 4.0, dc.y + 4.0)),
        );
        scene.stroke(
            &Stroke::new(1.5),
            base,
            dcol,
            None,
            &Line::new((dc.x - 4.0, dc.y + 4.0), (dc.x + 4.0, dc.y - 4.0)),
        );
        out.hits.push((
            SidebarHit::DeleteSignature(s.id.clone()),
            chrome::pointer_target(del, theme),
        ));
    }

    // ---- floating page bubble ---------------------------------------

    /// The floating page-info overlay: a small `[‹] Page X of Y [›]` pill that
    /// floats over the bottom-centre of the canvas. It appears on a page change
    /// (poked from `draw_all`) or while the pointer hovers the bottom-centre
    /// band, then fades — replacing the old always-on bottom status strip
    /// (the shared [`ui::StatusBubble`] pattern; markup counts already live in
    /// the sidebar tabs, so only the page indicator remains here). The pill's
    /// chevrons step pages; its rects are captured for hover + click.
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
        let text_size = self.type_px.sm;
        let chev = 26.0;
        let (pill_w, pill_h) = ui::status_bubble::pill_size(&label, tr, text_size, chev);
        let cx = f64::midpoint(canvas.x0, canvas.x1);
        let by = canvas.y1 - 16.0 - pill_h;
        let rect = Rect::new(cx - pill_w / 2.0, by, cx + pill_w / 2.0, by + pill_h);

        // Reveal zone: a band across the bottom-centre of the canvas (plus the
        // pill itself), so approaching the bottom edge surfaces the indicator.
        let zone = Rect::new(
            rect.x0 - 40.0,
            canvas.y1 - (pill_h + 32.0),
            rect.x1 + 40.0,
            canvas.y1,
        );
        let over = self.shots_page_bubble
            || zone.contains(self.last_mouse)
            || rect.contains(self.last_mouse);
        self.page_bubble.set_hover(over, now);

        let alpha = self.page_bubble.alpha(now);
        if alpha <= 0.0 {
            return;
        }

        // Capture geometry for hover + click hit-testing (the chevron slots).
        self.bubble_rect = rect;
        self.bubble_prev = Rect::new(rect.x0, rect.y0, rect.x0 + chev + 8.0, rect.y1);
        self.bubble_next = Rect::new(rect.x1 - chev - 8.0, rect.y0, rect.x1, rect.y1);

        scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            alpha,
            base,
            &rect.inflate(12.0, 14.0),
        );
        ui::status_bubble::draw_pill(scene, base, tr, theme, rect, &label, text_size);
        let prev_hot = self.bubble_prev.contains(self.last_mouse);
        let next_hot = self.bubble_next.contains(self.last_mouse);
        let cyy = f64::midpoint(rect.y0, rect.y1);
        let pc = Rect::new(rect.x0 + 8.0, cyy - 8.0, rect.x0 + 8.0 + 16.0, cyy + 8.0);
        let nc = Rect::new(rect.x1 - 8.0 - 16.0, cyy - 8.0, rect.x1 - 8.0, cyy + 8.0);
        ui::status_bubble::draw_chevron(
            scene,
            base,
            pc,
            if prev_hot {
                theme.text_primary
            } else {
                theme.text_dim
            },
            true,
        );
        ui::status_bubble::draw_chevron(
            scene,
            base,
            nc,
            if next_hot {
                theme.text_primary
            } else {
                theme.text_dim
            },
            false,
        );
        scene.pop_layer();
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
        Swatch::Custom(_) => "Custom",
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
            let body = Line::new(
                (r.x1, r.y0),
                (r.x0 + r.width() * 0.34, r.y1 - r.height() * 0.18),
            );
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
            scene.stroke(
                &Stroke::new(1.8),
                base,
                color,
                None,
                &RoundedRect::from_rect(sq, 2.0),
            );
            for i in 0..3 {
                let ly = sq.y0 + 4.0 + f64::from(i) * 4.0;
                scene.stroke(
                    &Stroke::new(1.4),
                    base,
                    color,
                    None,
                    &Line::new((sq.x0 + 3.0, ly), (sq.x1 - 3.0, ly)),
                );
            }
        }
        Tool::Text => {
            // A capital T in a text box.
            let bar = Line::new((r.x0 + 1.0, r.y0 + 2.0), (r.x1 - 1.0, r.y0 + 2.0));
            let stem = Line::new(
                (f64::midpoint(r.x0, r.x1), r.y0 + 2.0),
                (f64::midpoint(r.x0, r.x1), r.y1 - 1.0),
            );
            scene.stroke(&Stroke::new(2.2), base, color, None, &bar);
            scene.stroke(&Stroke::new(2.2), base, color, None, &stem);
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
            scene.stroke(
                &Stroke::new(1.4),
                base,
                color,
                None,
                &Line::new((r.x0, r.y1 + 1.0), (r.x1, r.y1 + 1.0)),
            );
        }
    }
}

fn draw_plus(scene: &mut Scene, base: Affine, btn: Rect, color: Color) {
    let c = Point::new(f64::midpoint(btn.x0, btn.x1), f64::midpoint(btn.y0, btn.y1));
    let l = 6.0;
    scene.stroke(
        &Stroke::new(2.0),
        base,
        color,
        None,
        &Line::new((c.x - l, c.y), (c.x + l, c.y)),
    );
    scene.stroke(
        &Stroke::new(2.0),
        base,
        color,
        None,
        &Line::new((c.x, c.y - l), (c.x, c.y + l)),
    );
}

fn draw_minus(scene: &mut Scene, base: Affine, btn: Rect, color: Color) {
    let c = Point::new(f64::midpoint(btn.x0, btn.x1), f64::midpoint(btn.y0, btn.y1));
    let l = 6.0;
    scene.stroke(
        &Stroke::new(2.0),
        base,
        color,
        None,
        &Line::new((c.x - l, c.y), (c.x + l, c.y)),
    );
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
    let cx = f64::midpoint(r.x0, r.x1);
    // Down arrow (shaft + head) in the upper two-thirds.
    let top = r.y0 + 1.0;
    let mid = r.y0 + r.height() * 0.52;
    scene.stroke(
        &Stroke::new(1.6),
        base,
        color,
        None,
        &Line::new((cx, top), (cx, mid)),
    );
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
    let c = Point::new(
        f64::midpoint(btn.x0, btn.x1) - 1.5,
        f64::midpoint(btn.y0, btn.y1) - 1.5,
    );
    scene.stroke(&Stroke::new(1.8), base, color, None, &Circle::new(c, 5.0));
    scene.stroke(
        &Stroke::new(1.8),
        base,
        color,
        None,
        &Line::new((c.x + 3.6, c.y + 3.6), (c.x + 7.0, c.y + 7.0)),
    );
}

fn draw_panel_glyph(scene: &mut Scene, base: Affine, btn: Rect, color: Color) {
    let r = icon_inset(btn);
    scene.stroke(
        &Stroke::new(1.6),
        base,
        color,
        None,
        &RoundedRect::from_rect(r, 2.0),
    );
    let divx = r.x0 + r.width() * 0.62;
    scene.stroke(
        &Stroke::new(1.6),
        base,
        color,
        None,
        &Line::new((divx, r.y0), (divx, r.y1)),
    );
}

/// Left-rail toggle glyph: a panel with the divider near the *left* edge,
/// mirroring the right-anchored sidebar glyph so the two read distinctly.
fn draw_rail_glyph(scene: &mut Scene, base: Affine, btn: Rect, color: Color) {
    let r = icon_inset(btn);
    scene.stroke(
        &Stroke::new(1.6),
        base,
        color,
        None,
        &RoundedRect::from_rect(r, 2.0),
    );
    let divx = r.x0 + r.width() * 0.38;
    scene.stroke(
        &Stroke::new(1.6),
        base,
        color,
        None,
        &Line::new((divx, r.y0), (divx, r.y1)),
    );
}

/// Draw the active inline-editor [`TextInput`] so its wrapped text lands at
/// `text_origin` with wrap width `wrap`. The multiline widget insets its
/// content by `ML_PAD` (10px horizontal, 8px vertical), so the field rect is
/// expanded by that inset to align the first glyph with where the static text
/// draws. The widget renders its own caret + selection highlight; the app
/// draws the surrounding focus box.
/// Position + draw the shared inline `TextInput` widget so it lands exactly
/// where the static text it replaces would sit. `content_size` is
/// `(wrap_width, text_height)`.
fn draw_inline_editor(
    input: &mut TextInput,
    scene: &mut Scene,
    base: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    text_origin: Point,
    content_size: (f32, f64),
) {
    // Mirror `ui::widget::text_input`'s multiline content inset so the widget's
    // first glyph aligns with the static text position.
    const ML_PAD_X: f64 = 10.0;
    const ML_PAD_Y: f64 = 8.0;

    let (wrap, text_h) = content_size;
    let rect = Rect::new(
        text_origin.x - ML_PAD_X,
        text_origin.y - ML_PAD_Y,
        text_origin.x + f64::from(wrap) + ML_PAD_X,
        text_origin.y + text_h.max(15.0) + ML_PAD_Y,
    );
    input.apply_layout_external(rect);
    input.draw(scene, base, tr, theme, &PresenceCtx::empty());
}

fn draw_avatar(
    scene: &mut Scene,
    base: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    name: &str,
    center: Point,
    radius: f64,
) {
    // Single-initial commenter disc, painted by the shared toolkit avatar so
    // the hue (deterministic per initial) + shape + centred glyph stay
    // consistent with every other app's roster.
    let rect = Rect::new(
        center.x - radius,
        center.y - radius,
        center.x + radius,
        center.y + radius,
    );
    let initial: String = name
        .chars()
        .next()
        .map_or_else(|| "?".to_string(), |c| c.to_string());
    let color = Avatar::color_for_initials(&initial, theme);
    Avatar::draw_at(scene, base, rect, &initial, color, theme, tr);
}
