//! The places panel: its dock and width, its groups and rows, the results still arriving, and the explore panel beside it.

use super::*;

impl MapsApp {
    /// The places panel: a floating panel down the leading edge (a drawer in
    /// a narrow window) with a header row — Back in Directions, the title,
    /// search and the collapse toggle — over the scrolled content. Collapsed
    /// it is just its header.
    #[allow(
        clippy::too_many_lines,
        reason = "one draw pass over the panel's header and its clipped, scrolled content"
    )]
    pub(super) fn draw_panel(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let pr = self.panel_rect();
        let sp = &theme.tokens.spacing;
        let (s1, s2) = (f64::from(sp.s1), f64::from(sp.s2));
        let header_h = f64::from(theme.tokens.metrics.bar_height);
        if pr.height() < header_h + 2.0 * s2 {
            return;
        }
        let docked = !self.overlay_mode();
        let collapsed = self.panel_collapsed && docked;
        let directions = matches!(self.model.panel, Panel::Directions) && !collapsed;
        let body = if collapsed {
            Rect::new(pr.x0, pr.y0, pr.x1, pr.y0 + header_h + 2.0 * s1)
        } else {
            pr
        };
        self.floats(body);
        ui::widget::floating_surface::paint(scene, t, body, theme);
        self.click_targets.push((body, Hit::Consume));
        let ink = on_panel(theme);

        // Header row.
        let header = Rect::new(
            body.x0 + s1,
            body.y0 + s1,
            body.x1 - s1,
            body.y0 + s1 + header_h,
        );
        let side = ButtonFace::height(ControlSize::Regular, theme);
        let button_at = |x0: f64| {
            Rect::new(
                x0,
                header.center().y - side / 2.0,
                x0 + side,
                header.center().y + side / 2.0,
            )
        };
        let title_x = if directions {
            let back = button_at(header.x0 + s1);
            self.icon_button(
                &mut PaintCtx::new(scene, t, tr, &ink),
                back,
                (Icon::ChevronLeft, "Back to places"),
                ButtonRole::Plain,
                false,
                Hit::PanelBack,
            );
            back.x1 + s1
        } else {
            header.x0 + f64::from(sp.s3)
        };
        let mut trailing = header.x1 - s1;
        if docked {
            let toggle = button_at(trailing - side);
            let (icon, name) = if collapsed {
                (Icon::ChevronDown, "Expand panel")
            } else {
                (Icon::ChevronUp, "Collapse panel")
            };
            self.icon_button(
                &mut PaintCtx::new(scene, t, tr, &ink),
                toggle,
                (icon, name),
                ButtonRole::Plain,
                false,
                Hit::ToggleCollapse,
            );
            trailing = toggle.x0;
            let search = button_at(trailing - side);
            self.icon_button(
                &mut PaintCtx::new(scene, t, tr, &ink),
                search,
                (Icon::Search, "Search places"),
                ButtonRole::Plain,
                false,
                Hit::OpenSearch,
            );
            trailing = search.x0;
        }
        let title = if matches!(self.model.panel, Panel::Directions) {
            "Directions"
        } else {
            "Places"
        };
        let heading = theme.text(TextRole::Heading);
        let mut ctx = PaintCtx::new(scene, t, tr, &ink);
        let top = draw::vmid(ctx.text, header.y0, header_h, &heading);
        draw::text_clip(
            &mut ctx,
            title,
            Point::new(title_x, top),
            &heading,
            ink.text_primary,
            trailing - s2 - title_x,
        );
        if collapsed {
            self.content_view_h = 0.0;
            self.content_h = 0.0;
            return;
        }

        // Content region (below the header), scrolled, and clipped at the
        // last block that fits whole, so the bottom never shows a sliver.
        let content = Rect::new(pr.x0 + s2, header.y1 + s1, pr.x1 - s2, pr.y1 - s2);
        self.content_view_h = content.height();
        // Measure this frame's rows before choosing the clip. The previous
        // frame's edges may belong to a different scale, mode or itinerary.
        let top = content.y0 - self.scroll;
        self.panel_origin = top;
        self.panel_edges.clear();
        let first_body_target = self.click_targets.len();
        let mut body_scene = Scene::new();
        let drawn = match self.model.panel {
            Panel::Explore => self.draw_explore(&mut body_scene, t, tr, &ink, content, top),
            Panel::Directions => self.draw_directions(&mut body_scene, t, tr, &ink, content, top),
        };
        self.content_h = drawn - top;
        let visible = self.scroll + content.height();
        let clip_bottom = if self.content_h > visible + 0.5 {
            self.panel_edges
                .iter()
                .copied()
                .filter(|edge| *edge > self.scroll && *edge <= visible + 0.5)
                .fold(None, |best: Option<f64>, edge| {
                    Some(best.map_or(edge, |b| b.max(edge)))
                })
                .map_or(content.y1, |edge| content.y0 + edge - self.scroll)
        } else {
            content.y1
        };
        let clip = Rect::new(content.x0, content.y0, content.x1, clip_bottom);
        // A clipped row is neither clickable nor published to accessibility.
        let body_targets = self.click_targets.split_off(first_body_target);
        self.click_targets
            .extend(body_targets.into_iter().filter(|(rect, _)| {
                rect.x0 >= clip.x0 && rect.x1 <= clip.x1 && rect.y0 >= clip.y0 && rect.y1 <= clip.y1
            }));
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &clip);
        scene.append(&body_scene, None);
        scene.pop_layer();
    }

    /// A group label inside the panel, with an optional trailing text action
    /// on its line. Returns the y below it.
    pub(super) fn draw_group_header(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        content: Rect,
        y: f64,
        title: &str,
        action: Option<(&str, Hit)>,
    ) -> f64 {
        let theme = ctx.theme;
        let sp = &theme.tokens.spacing;
        let (s1, s3) = (f64::from(sp.s1), f64::from(sp.s3));
        let style = theme.text(TextRole::Subheading);
        let line = draw::line_h(ctx.text, &style);
        let action_h = ButtonFace::height(ControlSize::Small, theme);
        let band_h = line.max(if action.is_some() { action_h } else { 0.0 });
        let band = Rect::new(content.x0, y, content.x1, y + band_h);
        let top = draw::vmid(ctx.text, band.y0, band_h, &style);
        let action_w = action.as_ref().map_or(0.0, |(label, _)| {
            ButtonFace::new(Rect::ZERO, label)
                .role(ButtonRole::Plain)
                .control_size(ControlSize::Small)
                .intrinsic_width(ctx.text, theme)
                + s1
        });
        draw::text_clip(
            ctx,
            title,
            Point::new(content.x0 + s3, top),
            &style,
            theme.text_primary,
            content.width() - 2.0 * s3 - action_w,
        );
        if let Some((label, hit)) = action {
            let face = ButtonFace::new(Rect::ZERO, label)
                .role(ButtonRole::Plain)
                .control_size(ControlSize::Small);
            let w = face.intrinsic_width(ctx.text, theme);
            let rect = Rect::new(
                content.x1 - w,
                band.center().y - action_h / 2.0,
                content.x1,
                band.center().y + action_h / 2.0,
            );
            self.text_button(
                ctx,
                ButtonFace::new(rect, label)
                    .role(ButtonRole::Plain)
                    .control_size(ControlSize::Small),
                rect,
                Some(hit),
            );
        }
        band.y1 + s1
    }

    /// Scroll the panel one block per wheel step, so its first visible row
    /// is always whole.
    pub(super) fn scroll_panel(&mut self, dy: f64) {
        let max = (self.content_h - self.content_view_h).max(0.0);
        let edges = std::iter::once(0.0).chain(self.panel_edges.iter().copied());
        let next = if dy < 0.0 {
            edges
                .filter(|edge| *edge > self.scroll + 0.5)
                .min_by(f64::total_cmp)
                .unwrap_or(max)
        } else if dy > 0.0 {
            edges
                .filter(|edge| *edge < self.scroll - 0.5)
                .max_by(f64::total_cmp)
                .unwrap_or(0.0)
        } else {
            self.scroll
        };
        self.scroll = next.clamp(0.0, max);
    }

    /// Note that a block of the panel's content ends at `y`.
    pub(super) fn mark_edge(&mut self, y: f64) {
        self.panel_edges.push(y - self.panel_origin);
    }

    /// One list row in the panel. Returns the y below it.
    pub(super) fn draw_row(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        content: Rect,
        y: f64,
        face: ListRowFace<'_>,
        selected: bool,
        separator: bool,
        hit: Hit,
    ) -> f64 {
        let h = face.height(ctx.text, ctx.theme);
        let rect = Rect::new(content.x0, y, content.x1, y + h);
        let face = face.selected(selected).interaction(Interaction {
            hovered: self.hovered(rect),
            ..Interaction::default()
        });
        let face = if separator {
            face.separator(false)
        } else {
            face
        };
        face.paint(ctx, rect);
        self.click_targets.push((rect, hit));
        self.mark_edge(rect.y1);
        rect.y1
    }

    /// The geocoder's places on their way: rows of a result's kind, the
    /// first saying where they come from once they take a while, under the
    /// results (or their header, while there are none yet).
    pub(super) fn draw_arriving_results(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        content: Rect,
        mut y: f64,
    ) -> f64 {
        let Some(clock) = self.search_clock() else {
            return y;
        };
        if self.model.results.is_empty() {
            y = self.draw_group_header(
                &mut PaintCtx::new(scene, t, tr, theme),
                content,
                y,
                "Results",
                None,
            );
        }
        let step = clock.step(ui::time::now());
        let h = RESULT_ROW.height(tr, theme);
        let mut ctx = PaintCtx::new(scene, t, tr, theme);
        for row in 0..ARRIVING_RESULTS {
            ui::widget::ArrivalFace::at_step(
                ui::widget::SkeletonShape::Row(RESULT_ROW),
                step,
                clock.source(),
            )
            .naming(row == 0)
            .paint(&mut ctx, Rect::new(content.x0, y, content.x1, y + h));
            y += h;
        }
        self.mark_edge(y);
        y
    }

    /// The timing of the places a committed search waits on.
    pub(super) fn search_clock(&self) -> Option<ui::arriving::ArrivalClock> {
        let (_, asked) = self.search_asked.as_ref()?;
        Some(ui::arriving::ArrivalClock::new(
            *asked,
            ui::arriving::Source::host(GEOCODER),
        ))
    }

    /// The Explore view: the selected place, then results, nearby places,
    /// saved places and recent searches — or, with nothing to show, one
    /// sentence and the way to search. Returns the y below the content.
    #[allow(
        clippy::too_many_lines,
        reason = "a single linear draw pass over this panel's sections; splitting it would only \
              relocate the line count, not the amount a reader must hold in mind"
    )]
    pub(super) fn draw_explore(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        content: Rect,
        mut y: f64,
    ) -> f64 {
        let sp = &theme.tokens.spacing;
        let (s2, s3, s4) = (f64::from(sp.s2), f64::from(sp.s3), f64::from(sp.s4));
        let anchor = self.search_anchor();
        let distance = |pos: LatLon| geo::fmt_distance(geo::haversine_m(pos, anchor));
        let results_anchor = self.results_anchor.unwrap_or(anchor);
        let result_distance =
            |pos: LatLon| geo::fmt_distance(geo::haversine_m(pos, results_anchor));

        // First run: nothing selected, searched, saved, recent, or fetched for
        // the viewport — one sentence and the one action that changes that.
        let empty = self.model.selected_place().is_none()
            && self.model.results.is_empty()
            && self.model.saved_places().is_empty()
            && self.model.recents.is_empty()
            && self.pois.is_empty();
        if empty {
            let rect = Rect::new(content.x0, y, content.x1, content.y1);
            let empty_state = EmptyStateContent {
                icon: Some(Icon::Search),
                action_label: Some("Search places"),
                ..EmptyStateContent::new("Search for a place or an address to see it here.")
            };
            let mut ctx = PaintCtx::new(scene, t, tr, theme);
            if let Some(action) = EmptyState::paint(&mut ctx, rect, &empty_state, self.last_mouse) {
                self.click_targets.push((action, Hit::OpenEmptySearch));
            }
            return content.y1;
        }

        // The selected place: an object card with what it is, where it is,
        // and what to do with it.
        if let Some(sel) = self.model.selected_place().cloned() {
            let card_rect_w = content.width() - 2.0 * s2;
            let face = CardFace::new().title(&sel.name);
            let caption = theme.text(TextRole::Caption);
            let caption_h = draw::line_h(tr, &caption);
            let button_h = ButtonFace::height(ControlSize::Regular, theme);
            let body_h = caption_h * 2.0 + s4 + button_h;
            let card_h = face.height_for_body(tr, theme, card_rect_w, body_h);
            let card = Rect::new(content.x0 + s2, y + s2, content.x1 - s2, y + s2 + card_h);
            let mut ctx = PaintCtx::new(scene, t, tr, theme);
            face.paint(&mut ctx, card);
            let body = face.body_rect(ctx.text, theme, card);
            draw::text(
                &mut ctx,
                sel.category.label(),
                Point::new(body.x0, body.y0),
                &caption,
                theme.text_dim,
            );
            draw::text_clip(
                &mut ctx,
                &sel.address,
                Point::new(body.x0, body.y0 + caption_h),
                &caption,
                theme.text_dim,
                body.width(),
            );
            // The actions: Directions is the card's one primary action.
            let by = body.y0 + caption_h * 2.0 + s4;
            let bw = (body.width() - 2.0 * s2) / 3.0;
            let saved = sel.saved;
            let actions: [(&str, Hit, ButtonRole); 3] = [
                ("Directions", Hit::Directions, ButtonRole::Primary),
                ("Transit", Hit::Transit, ButtonRole::Secondary),
                (
                    if saved { "Saved" } else { "Save" },
                    Hit::ToggleSave,
                    ButtonRole::Secondary,
                ),
            ];
            for (i, (label, hit, role)) in actions.into_iter().enumerate() {
                let bx = body.x0 + i as f64 * (bw + s2);
                let br = Rect::new(bx, by, bx + bw, by + button_h);
                let mut face = ButtonFace::new(br, label).role(role);
                if matches!(hit, Hit::ToggleSave) {
                    face = face.icon(Icon::Star).toggled(saved);
                }
                self.text_button(&mut PaintCtx::new(scene, t, tr, theme), face, br, Some(hit));
            }
            self.mark_edge(card.y1);
            y = card.y1 + s4;
        }

        // Results of a committed search.
        let results = self.model.results.clone();
        if !results.is_empty() {
            y = self.draw_group_header(
                &mut PaintCtx::new(scene, t, tr, theme),
                content,
                y,
                "Results",
                None,
            );
            let mut ctx = PaintCtx::new(scene, t, tr, theme);
            let last = results.len() - 1;
            for (i, p) in results.iter().enumerate() {
                let meta = result_distance(p.pos);
                let secondary = if p.address.is_empty() {
                    p.category.label()
                } else {
                    p.address.as_str()
                };
                let selected = self.model.selected.as_deref() == Some(p.id.as_str());
                y = self.draw_row(
                    &mut ctx,
                    content,
                    y,
                    ListRowFace::new(&p.name).secondary(secondary).meta(&meta),
                    selected,
                    i < last,
                    Hit::SelectPlace(p.id.clone()),
                );
            }
        }
        y = self.draw_arriving_results(scene, t, tr, theme, content, y);
        if !results.is_empty() || self.search_asked.is_some() {
            y += s3;
        }

        // Nearby POIs fetched for the current viewport (Overpass), nearest-first
        // and linked to the marker layer: tapping a row centres the map and
        // opens the POI's info card, and the selected marker selects its row.
        let nearby = self.nearby_pois();
        if !nearby.is_empty() {
            y = self.draw_group_header(
                &mut PaintCtx::new(scene, t, tr, theme),
                content,
                y,
                "Nearby",
                None,
            );
            let mut ctx = PaintCtx::new(scene, t, tr, theme);
            let last = nearby.len() - 1;
            for (i, poi) in nearby.iter().enumerate() {
                let meta = distance(poi.pos);
                let secondary = poi.kind_label();
                let selected = self.selected_poi.as_deref() == Some(poi.id.as_str());
                y = self.draw_row(
                    &mut ctx,
                    content,
                    y,
                    ListRowFace::new(&poi.name)
                        .secondary(&secondary)
                        .meta(&meta),
                    selected,
                    i < last,
                    Hit::SelectPoi(poi.id.clone()),
                );
            }
            y += s3;
        }

        // A saved place already listed under the results is not listed twice.
        let saved: Vec<Place> = self
            .model
            .saved_places()
            .into_iter()
            .filter(|place| !results.iter().any(|hit| hit.id == place.id))
            .cloned()
            .collect();
        if !saved.is_empty() {
            y = self.draw_group_header(
                &mut PaintCtx::new(scene, t, tr, theme),
                content,
                y,
                "Saved places",
                None,
            );
            let mut ctx = PaintCtx::new(scene, t, tr, theme);
            let last = saved.len() - 1;
            for (i, p) in saved.iter().enumerate() {
                let meta = distance(p.pos);
                let selected = self.model.selected.as_deref() == Some(p.id.as_str());
                y = self.draw_row(
                    &mut ctx,
                    content,
                    y,
                    ListRowFace::new(&p.name)
                        .secondary(p.category.label())
                        .meta(&meta),
                    selected,
                    i < last,
                    Hit::SelectPlace(p.id.clone()),
                );
            }
            y += s3;
        }

        if !self.model.recents.is_empty() {
            y = self.draw_group_header(
                &mut PaintCtx::new(scene, t, tr, theme),
                content,
                y,
                "Recent searches",
                Some(("Clear", Hit::ClearRecents)),
            );
            let recents = self.model.recents.clone();
            let mut ctx = PaintCtx::new(scene, t, tr, theme);
            let last = recents.len() - 1;
            for (i, q) in recents.iter().enumerate() {
                y = self.draw_row(
                    &mut ctx,
                    content,
                    y,
                    ListRowFace::new(q).leading(RowLeading::Icon(Icon::History)),
                    false,
                    i < last,
                    Hit::Recent(q.clone()),
                );
            }
            y += s3;
        }
        y
    }
}
