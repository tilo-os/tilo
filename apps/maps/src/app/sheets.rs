//! The directions and departures sheets, with the picker that sets when a trip leaves.

use super::*;

impl MapsApp {
    /// The Directions view: the From/To fields, the travel mode, the
    /// departure time, then the route (or, in Transit, the departures).
    /// Returns the y below the content.
    #[allow(
        clippy::too_many_lines,
        reason = "a single linear draw pass over this panel's fixed regions; splitting it would only \
              relocate the line count, not the amount a reader must hold in mind"
    )]
    pub(super) fn draw_directions(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        content: Rect,
        mut y: f64,
    ) -> f64 {
        let sp = &theme.tokens.spacing;
        let (s2, s3) = (f64::from(sp.s2), f64::from(sp.s3));
        // Controls sit an `s2` in from the content edge, the rows' own inset.
        let inner = Rect::new(content.x0 + s2, content.y0, content.x1 - s2, content.y1);
        let x = inner.x0;
        let right = inner.x1;

        let selected_destination = self.model.selected_place().map(|place| place.name.as_str());
        let frame = EndpointFrame {
            content: inner,
            top: y + s2,
            origin: self.model.origin.and_then(|_| {
                (!self.model.origin_name.is_empty()).then_some(self.model.origin_name.as_str())
            }),
            destination: (!self.model.dest_name.is_empty())
                .then_some(self.model.dest_name.as_str())
                .or(selected_destination),
        };
        let field_h = ButtonFace::height(ControlSize::Regular, theme);
        let loc = Rect::new(right - field_h, frame.top, right, frame.top + field_h);
        y = self.endpoints.draw_fields(scene, t, tr, theme, &frame);

        // Origin actions belong beside From; the swap action belongs beside
        // To. This leaves room for the itinerary instead of a third field row.
        let small_h = ButtonFace::height(ControlSize::Small, theme);
        let (loc_label, loc_hit) = if self.locating {
            (failure::t("maps-locating"), None)
        } else {
            (failure::t("maps-use-my-location"), Some(Hit::UseMyLocation))
        };
        self.text_button(
            &mut PaintCtx::new(scene, t, tr, theme),
            ButtonFace::icon_only(loc, Icon::LocateFixed, &loc_label)
                .role(ButtonRole::Plain)
                .disabled(self.locating),
            loc,
            loc_hit,
        );

        // Travel mode: the primary choice of the view.
        let seg_h = SegmentedFace::height(ControlSize::Regular, theme);
        let seg_w = (right - x) / TravelMode::ALL.len() as f64;
        let segs: Vec<Rect> = (0..TravelMode::ALL.len())
            .map(|i| {
                let sx = x + i as f64 * seg_w;
                Rect::new(sx, y, sx + seg_w, y + seg_h)
            })
            .collect();
        let labels: Vec<&str> = TravelMode::ALL.iter().map(|m| m.label()).collect();
        let current = TravelMode::ALL
            .iter()
            .position(|m| *m == self.model.mode)
            .unwrap_or(0);
        let hovered = segs.iter().position(|r| self.hovered(*r));
        ToggleGroup::paint(
            scene,
            t,
            tr,
            theme,
            &SegmentedFace::new(&segs, &labels, current).hovered(hovered),
        );
        for (rect, mode) in segs.iter().zip(TravelMode::ALL) {
            self.click_targets.push((*rect, Hit::Mode(mode)));
        }
        y += seg_h + s3;

        // Departure time: a quiet refinement under the mode.
        y = self.draw_time_picker(scene, t, tr, theme, inner, y);

        // Transit plans a public-transport journey instead of a road route.
        if self.model.mode.is_transit() {
            return self.draw_departures(scene, t, tr, theme, content, y);
        }

        let Some(route) = self.model.route.clone() else {
            let body = theme.text(TextRole::Body);
            let mut ctx = PaintCtx::new(scene, t, tr, theme);
            draw::text_clip(
                &mut ctx,
                &failure::t("maps-route-choose-destination"),
                Point::new(x, y),
                &body,
                theme.text_dim,
                right - x,
            );
            return y + draw::line_h(ctx.text, &body) + s2;
        };

        // The trip: how long it takes (the view's one hero number), how far,
        // and when it arrives.
        let display = theme.text(TextRole::Display).numeric();
        let body_numeric = theme.text(TextRole::Body).numeric();
        let duration = geo::fmt_duration(route.duration_s);
        let display_h = draw::line_h(tr, &display);
        let distance = geo::fmt_distance(route.distance_m);
        // The distance and Clear route stand on the duration's baseline.
        let baseline = y + draw::baseline(tr, &duration, &display);
        let clear_face = ButtonFace::new(Rect::ZERO, "Clear route")
            .role(ButtonRole::Plain)
            .control_size(ControlSize::Small);
        let clear_w = clear_face.intrinsic_width(tr, theme);
        let label = theme.text(TextRole::Label);
        let label_top = (small_h - draw::line_h(tr, &label)) / 2.0;
        let clear_y = baseline - label_top - draw::baseline(tr, "Clear route", &label);
        let clear = Rect::new(right - clear_w, clear_y, right, clear_y + small_h);
        self.text_button(
            &mut PaintCtx::new(scene, t, tr, theme),
            ButtonFace::new(clear, "Clear route")
                .role(ButtonRole::Plain)
                .control_size(ControlSize::Small),
            clear,
            Some(Hit::ClearRoute),
        );
        // Another route, when the service offered one: the trip's own number
        // says which of them is shown.
        if self.model.route_alternatives.len() > 1 {
            let shown = self
                .model
                .route
                .as_ref()
                .and_then(|current| {
                    self.model.route_alternatives.iter().position(|offered| {
                        offered.duration_s == current.duration_s
                            && offered.distance_m == current.distance_m
                    })
                })
                .unwrap_or(0);
            let label = format!(
                "Route {} of {}",
                shown + 1,
                self.model.route_alternatives.len()
            );
            let face = ButtonFace::new(Rect::ZERO, &label)
                .role(ButtonRole::Plain)
                .control_size(ControlSize::Small);
            let width = face.intrinsic_width(tr, theme);
            let row_y = clear_y - s2 - small_h;
            let rect = Rect::new(right - width, row_y, right, row_y + small_h);
            self.text_button(
                &mut PaintCtx::new(scene, t, tr, theme),
                ButtonFace::new(rect, &label)
                    .role(ButtonRole::Plain)
                    .control_size(ControlSize::Small),
                rect,
                Some(Hit::NextRoute),
            );
        }
        let mut ctx = PaintCtx::new(scene, t, tr, theme);
        draw::text(
            &mut ctx,
            &duration,
            Point::new(x, y),
            &display,
            theme.text_primary,
        );
        let duration_w = draw::measure_w(ctx.text, &duration, &display);
        let body_h = draw::line_h(ctx.text, &body_numeric);
        let distance_top = baseline - draw::baseline(ctx.text, &distance, &body_numeric);
        draw::text_clip(
            &mut ctx,
            &distance,
            Point::new(x + duration_w + s3, distance_top),
            &body_numeric,
            theme.text_dim,
            clear.x0 - s2 - (x + duration_w + s3),
        );
        y += display_h;
        let (departure, arrival) = self.road_trip_clock(route.duration_s);
        let trip = if self.model.transit_time.is_now() {
            format!("Arrive at {arrival}")
        } else {
            format!("Leave at {departure}, arrive at {arrival}")
        };
        draw::text(
            &mut ctx,
            &trip,
            Point::new(x, y),
            &body_numeric,
            theme.text_dim,
        );
        y += body_h + s3;

        // Summary and steps are two groups: a derived rule, then the steps.
        ui::surface::separator(
            ctx.scene,
            t,
            x,
            right,
            y,
            0.0,
            theme.tokens.colors.surfaces.content,
        );
        y += s2;
        let last = route.steps.len().saturating_sub(1);
        for (i, step) in route.steps.iter().enumerate() {
            let meta = if step.distance_m >= 1.0 {
                geo::fmt_distance(step.distance_m)
            } else {
                String::new()
            };
            let mut face =
                ListRowFace::new(&step.instruction).leading(RowLeading::Icon(step.maneuver.icon()));
            if !meta.is_empty() {
                face = face.meta(&meta);
            }
            let h = face.height(ctx.text, theme);
            let rect = Rect::new(content.x0, y, content.x1, y + h);
            let face = if i < last {
                face.separator(false)
            } else {
                face
            };
            face.paint(&mut ctx, rect);
            self.panel_edges.push(rect.y1 - self.panel_origin);
            y = rect.y1;
        }
        y + s3
    }

    /// The planned departure and arrival clocks for a road trip of
    /// `duration_s`, honouring the leave/arrive picker: "Leave" anchors the
    /// departure (arrival = departure + duration); "Arrive" anchors the
    /// arrival (departure = arrival − duration).
    pub(super) fn road_trip_clock(&self, duration_s: f64) -> (String, String) {
        let tt = self.model.transit_time;
        let dur = chrono::Duration::seconds(duration_s.max(0.0) as i64);
        let anchor = ui::clock::local_now() + chrono::Duration::minutes(i64::from(tt.offset_min));
        let (dep, arr) = if tt.arrive_by() {
            (anchor - dur, anchor)
        } else {
            (anchor, anchor + dur)
        };
        (clock_label(dep), clock_label(arr))
    }

    /// The departure time: a quiet Leave/Arrive choice and a stepper that
    /// moves the time in quarter hours, with "Now" to go back. Defaults to
    /// leaving now. Returns the y below it.
    pub(super) fn draw_time_picker(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        inner: Rect,
        y: f64,
    ) -> f64 {
        let sp = &theme.tokens.spacing;
        let (s1, s4) = (f64::from(sp.s1), f64::from(sp.s4));
        let x = inner.x0;
        let right = inner.x1;
        let tt = self.model.transit_time;
        let row_h = SegmentedFace::height(ControlSize::Regular, theme);

        // Leave / Arrive: quiet, so the travel mode above reads first.
        let labels = ["Leave", "Arrive"];
        let toggle_w = SegmentedFace::intrinsic_width(&labels, ControlSize::Regular, tr, theme);
        let half = toggle_w / 2.0;
        let segs = [
            Rect::new(x, y, x + half, y + row_h),
            Rect::new(x + half, y, x + toggle_w, y + row_h),
        ];
        let current = usize::from(tt.mode == TransitTimeMode::Arrive);
        let hovered = segs.iter().position(|r| self.hovered(*r));
        ToggleGroup::paint(
            scene,
            t,
            tr,
            theme,
            &SegmentedFace::new(&segs, &labels, current)
                .style(SegmentedStyle::Quiet)
                .hovered(hovered),
        );
        self.click_targets
            .push((segs[0], Hit::TransitTimeMode(TransitTimeMode::Depart)));
        self.click_targets
            .push((segs[1], Hit::TransitTimeMode(TransitTimeMode::Arrive)));

        // The time stepper: earlier, the time, later. Wide enough for any
        // clock time, so stepping never moves it.
        let target = ui::clock::local_now() + chrono::Duration::minutes(i64::from(tt.offset_min));
        let label = if tt.is_now() {
            "Now".to_string()
        } else {
            clock_label(target)
        };
        let size = ControlSize::Regular;
        let height = NumberStepperFace::height(size, theme);
        let width = NumberStepperFace::width(size, &["00:00", &label], tr, theme);
        let stepper = Rect::new(
            right - width,
            y + (row_h - height) / 2.0,
            right,
            y + (row_h + height) / 2.0,
        );
        let probe = NumberStepperFace::new(stepper, &label).names("Earlier", "Later");
        let hovered = probe
            .part_at(theme, self.last_mouse)
            .filter(|_| !self.dragging);
        probe
            .hovered(hovered)
            .paint(&mut PaintCtx::new(scene, t, tr, theme));
        let (earlier, later) = probe.hit_rects(theme);
        self.click_targets
            .push((earlier, Hit::TransitTimeDelta(-15)));
        self.click_targets.push((later, Hit::TransitTimeDelta(15)));
        let mut y = y + row_h;

        // A way back to "now" appears only when the time is off it.
        if !tt.is_now() {
            y += s1;
            let small_h = ButtonFace::height(ControlSize::Small, theme);
            let face = ButtonFace::new(Rect::ZERO, "Reset to now")
                .role(ButtonRole::Plain)
                .control_size(ControlSize::Small);
            let w = face.intrinsic_width(tr, theme);
            let reset = Rect::new(x, y, x + w, y + small_h);
            self.text_button(
                &mut PaintCtx::new(scene, t, tr, theme),
                ButtonFace::new(reset, "Reset to now")
                    .role(ButtonRole::Plain)
                    .control_size(ControlSize::Small),
                reset,
                Some(Hit::TransitTimeNow),
            );
            y += small_h;
        }
        y + s4
    }

    /// The public-transport departures for Transit: one row per journey (its
    /// times, length, changes and lines), the chosen one opened into its
    /// legs, and where the times come from.
    #[allow(
        clippy::too_many_lines,
        reason = "a single linear draw pass over this panel's fixed regions; splitting it would only \
              relocate the line count, not the amount a reader must hold in mind"
    )]
    #[allow(
        clippy::many_single_char_names,
        reason = "the shared drawing names (t: transform, x/y: the pen, s1–s4: spacing steps) \
              used consistently across every draw_* method in this file"
    )]
    pub(super) fn draw_departures(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        content: Rect,
        mut y: f64,
    ) -> f64 {
        let sp = &theme.tokens.spacing;
        let (s1, s2, s3) = (f64::from(sp.s1), f64::from(sp.s2), f64::from(sp.s3));
        let to = if self.model.dest_name.is_empty() {
            self.model
                .selected_place()
                .map_or_else(|| "your destination".to_string(), |p| p.name.clone())
        } else {
            self.model.dest_name.clone()
        };
        y = self.draw_group_header(
            &mut PaintCtx::new(scene, t, tr, theme),
            content,
            y,
            &format!("Departures to {to}"),
            None,
        );

        if self.model.transit.is_empty() {
            let has_target = self.model.dest.is_some() || self.model.selected_place().is_some();
            let msg = if self.pending > 0 {
                "Finding departures…"
            } else if !has_target {
                "Choose a destination to see departures."
            } else {
                "No public transport goes there at this time."
            };
            let body = theme.text(TextRole::Body);
            let mut ctx = PaintCtx::new(scene, t, tr, theme);
            draw::text_clip(
                &mut ctx,
                msg,
                Point::new(content.x0 + s2 + s3, y),
                &body,
                theme.text_dim,
                content.width() - 2.0 * (s2 + s3),
            );
            return y + draw::line_h(ctx.text, &body) + s2;
        }

        let title_style = theme.text(TextRole::Body).strong().numeric();
        let caption = theme.text(TextRole::Caption);
        let numeric = caption.clone().numeric();
        let badge = theme.text(TextRole::Caption).strong();
        let title_h = draw::line_h(tr, &title_style);
        let caption_h = draw::line_h(tr, &caption);
        let chip_h = caption_h + s1;
        let pad_x = s3;
        let journeys = self.model.transit.clone();
        let ground = theme.tokens.colors.surfaces.content;
        for (i, j) in journeys.iter().enumerate() {
            let selected = self.model.transit_choice == i;
            let row_h = s2 + title_h + s1 + chip_h + s2;
            let row = Rect::new(content.x0, y, content.x1, y + row_h);
            ui::chrome::paint_row_selection(
                scene,
                t,
                row,
                SelectionState::new(selected, self.hovered(row), false),
                ground,
                theme,
            );
            let mut ctx = PaintCtx::new(scene, t, tr, theme);
            let x = row.x0 + pad_x;
            let right = row.x1 - pad_x;
            let ty = row.y0 + s2;
            draw::text(
                &mut ctx,
                &format!("{}–{}", j.dep, j.arr),
                Point::new(x, ty),
                &title_style,
                theme.text_primary,
            );
            let meta_top = ty + (title_h - caption_h) / 2.0;
            draw::text_right(
                &mut ctx,
                &format!("{} min", j.duration_min),
                right,
                meta_top,
                &numeric,
                theme.text_dim,
            );
            // The journey's lines in their own colours, walks as plain text.
            let chips_y = ty + title_h + s1;
            let changes = match j.transfers {
                0 => "Direct".to_string(),
                1 => "1 change".to_string(),
                n => format!("{n} changes"),
            };
            let changes_w = draw::measure_w(ctx.text, &changes, &caption);
            draw::text_right(
                &mut ctx,
                &changes,
                right,
                chips_y + (chip_h - caption_h) / 2.0,
                &caption,
                theme.text_dim,
            );
            let chips_right = right - changes_w - s3;
            let mut px = x;
            let sep = f64::from(theme.tokens.metrics.icon_sm);
            for (li, leg) in j.legs.iter().enumerate() {
                let lead = if li > 0 { sep + s1 } else { 0.0 };
                if leg.mode == TransitMode::Walk {
                    let label = format!("Walk {} min", leg.duration_min.max(1));
                    let w = draw::measure_w(ctx.text, &label, &numeric);
                    if px + lead + w > chips_right {
                        break;
                    }
                    if li > 0 {
                        let r = Rect::new(
                            px,
                            chips_y + (chip_h - sep) / 2.0,
                            px + sep,
                            chips_y + (chip_h + sep) / 2.0,
                        );
                        Icon::ChevronRight.draw(ctx.scene, t, r, theme.text_tertiary);
                        px += lead;
                    }
                    draw::text(
                        &mut ctx,
                        &label,
                        Point::new(px, chips_y + (chip_h - caption_h) / 2.0),
                        &numeric,
                        theme.text_dim,
                    );
                    px += w + s1;
                } else {
                    let col = canvas::leg_color(theme, leg);
                    let fg = leg
                        .text_color
                        .as_deref()
                        .and_then(draw::hex_color)
                        .unwrap_or_else(|| theme.tokens.colors.on_surface(col).ink);
                    let label = if leg.line.is_empty() {
                        leg.mode.label().to_string()
                    } else {
                        leg.line.clone()
                    };
                    let w = draw::measure_w(ctx.text, &label, &badge) + 2.0 * s2;
                    if px + lead + w > chips_right {
                        break;
                    }
                    if li > 0 {
                        let r = Rect::new(
                            px,
                            chips_y + (chip_h - sep) / 2.0,
                            px + sep,
                            chips_y + (chip_h + sep) / 2.0,
                        );
                        Icon::ChevronRight.draw(ctx.scene, t, r, theme.text_tertiary);
                        px += lead;
                    }
                    let pill = Rect::new(px, chips_y, px + w, chips_y + chip_h);
                    // A line badge in the operator's own colour is content,
                    // like a calendar's event colour.
                    draw::fill_round(
                        ctx.scene,
                        t,
                        pill,
                        f64::from(theme.tokens.radii.detail),
                        col,
                    );
                    draw::text_center(
                        &mut ctx,
                        &label,
                        Point::new(pill.center().x, chips_y + (chip_h - caption_h) / 2.0),
                        &badge,
                        fg,
                    );
                    px += w + s1;
                }
            }
            self.click_targets.push((row, Hit::Journey(i)));
            self.mark_edge(row.y1);
            y = row.y1 + s1;

            // The chosen journey opened into its legs.
            if selected {
                let mut ctx = PaintCtx::new(scene, t, tr, theme);
                for leg in &j.legs {
                    let col = canvas::leg_color(theme, leg);
                    let (title, secondary) = if leg.mode == TransitMode::Walk {
                        (
                            format!("Walk to {}", leg.to_name),
                            format!("{} min", leg.duration_min.max(1)),
                        )
                    } else {
                        let line = format!("{} {}", leg.mode.label(), leg.line);
                        let title = if leg.headsign.is_empty() {
                            line
                        } else {
                            format!("{line} towards {}", leg.headsign)
                        };
                        let stops = if leg.num_stops == 1 {
                            "1 stop".to_string()
                        } else {
                            format!("{} stops", leg.num_stops)
                        };
                        (
                            title,
                            format!("{} to {}, {stops}", leg.from_name, leg.to_name),
                        )
                    };
                    let times = format!("{}–{}", leg.dep, leg.arr);
                    let live = [("Live", ChipStyle::Tinted(Tone::Success))];
                    let face = ListRowFace::new(&title)
                        .secondary(&secondary)
                        .meta(&times)
                        .tags(if leg.realtime { &live } else { &[] });
                    let h = face.height(ctx.text, theme);
                    let rect = Rect::new(content.x0, y, content.x1, y + h);
                    // The leg's line colour down its leading edge ties the
                    // row to the line drawn on the map.
                    let stripe_x = rect.x0 + s1;
                    draw::line(
                        ctx.scene,
                        t,
                        Point::new(stripe_x, rect.y0 + s1),
                        Point::new(stripe_x, rect.y1 - s1),
                        f64::from(theme.tokens.metrics.chrome_rule_px) * 3.0,
                        col,
                    );
                    face.paint(&mut ctx, rect);
                    self.panel_edges.push(rect.y1 - self.panel_origin);
                    y = rect.y1;
                }
                y += s2;
            }
        }

        // Where the times come from, and whether they are live: Transitous
        // runs over the operators' aggregated GTFS(-RT) feeds; a leg flagged
        // real-time is live, a schedule-only feed reads "Scheduled".
        let agency = journeys
            .iter()
            .flat_map(|j| j.legs.iter())
            .find(|l| !l.agency.is_empty())
            .map(|l| l.agency.clone());
        let any_live = journeys
            .iter()
            .flat_map(|j| j.legs.iter())
            .any(|l| l.realtime);
        let freshness = if any_live {
            "Live times"
        } else {
            "Scheduled times"
        };
        let source = match agency {
            Some(a) => format!("{freshness} from {a} via Transitous"),
            None => format!("{freshness} via Transitous"),
        };
        let mut ctx = PaintCtx::new(scene, t, tr, theme);
        y += s2;
        draw::text_clip(
            &mut ctx,
            &source,
            Point::new(content.x0 + s3, y),
            &caption,
            theme.text_dim,
            content.width() - 2.0 * s3,
        );
        y + caption_h + s2
    }
}
