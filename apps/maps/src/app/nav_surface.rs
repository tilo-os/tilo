//! The floating navigation surface a journey draws in, and the card for the place a person selected.

use super::*;

impl MapsApp {
    /// The live turn card: a floating panel with the next instruction and
    /// the trip summary, dragged out to the desktop by its title row.
    #[allow(
        clippy::too_many_lines,
        reason = "the navigation card is one cohesive painted surface with shared geometry for its drag and end-navigation targets"
    )]
    pub(super) fn draw_navigation_surface(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let Some(route) = self.model.route.clone() else {
            self.navigation_drag_handle = Rect::ZERO;
            self.navigation_end_rect = Rect::ZERO;
            self.navigation_end_hovered = false;
            return;
        };
        let rect = self.navigation_rect();
        self.floats(rect);
        let visual =
            self.navigation_placement
                .presentation_transform(transform, rect, theme.reduce_motion);
        ui::widget::floating_surface::paint(scene, visual, rect, theme);
        let ink = on_panel(theme);
        let sp = &theme.tokens.spacing;
        let (s2, s3) = (f64::from(sp.s2), f64::from(sp.s3));
        let pad = s3;
        let numeric_caption = theme.text(TextRole::Caption).numeric();
        let heading = theme.text(TextRole::Heading);
        let body_numeric = theme.text(TextRole::Body).numeric();
        let heading_h = draw::line_h(tr, &heading);

        // The next instruction: its manoeuvre icon and the instruction across
        // the card's width; the distance to it on the line below.
        let end_face = ButtonFace::new(Rect::ZERO, "End").control_size(ControlSize::Small);
        let end_w = end_face.intrinsic_width(tr, theme);
        let end_h = ButtonFace::height(ControlSize::Small, theme);
        let row = Rect::new(
            rect.x0 + pad,
            rect.y0 + pad,
            rect.x1 - pad,
            rect.y0 + pad + heading_h,
        );
        // The instruction rows are the drag handle; End stays a plain press.
        let mut ctx = PaintCtx::new(scene, visual, tr, &ink);
        ui::surface::floating_drag_grip(ctx.scene, visual, rect, ink.text_tertiary);
        let instruction = route.steps.first();
        let icon = instruction.map_or(Icon::ArrowUp, |step| step.maneuver.icon());
        let text = instruction.map_or("Continue on the route", |step| step.instruction.as_str());
        let icon_side = f64::from(theme.tokens.metrics.icon_lg);
        let icon_rect = Rect::new(
            row.x0,
            row.center().y - icon_side / 2.0,
            row.x0 + icon_side,
            row.center().y + icon_side / 2.0,
        );
        icon.draw(ctx.scene, visual, icon_rect, ink.text_primary);
        let text_x = icon_rect.x1 + s3;
        draw::text_clip(
            &mut ctx,
            text,
            Point::new(text_x, row.y0),
            &heading,
            ink.text_primary,
            row.x1 - text_x,
        );
        let next_distance = instruction.map_or(route.distance_m, |step| step.distance_m);
        let dist_y = row.y1;
        draw::text(
            &mut ctx,
            &geo::fmt_distance(next_distance),
            Point::new(text_x, dist_y),
            &body_numeric,
            ink.text_dim,
        );
        let rule_y = dist_y + draw::line_h(ctx.text, &body_numeric) + s3;
        self.navigation_drag_handle = Rect::new(rect.x0, rect.y0, rect.x1, rule_y);
        ui::surface::separator(
            ctx.scene,
            visual,
            rect.x0 + pad,
            rect.x1 - pad,
            rule_y,
            0.0,
            ink.tokens.colors.surfaces.content,
        );

        // The summary: the time to go and, on its baseline, the distance
        // left; End at its trailing edge.
        let summary = Rect::new(
            rect.x0 + pad,
            rule_y + s3,
            rect.x1 - pad,
            rule_y + s3 + heading_h.max(end_h),
        );
        let duration = geo::fmt_duration(route.duration_s);
        let duration_style = heading.clone().numeric();
        let summary_y = summary.center().y - heading_h / 2.0;
        draw::text(
            &mut ctx,
            &duration,
            Point::new(summary.x0, summary_y),
            &duration_style,
            ink.text_primary,
        );
        let remaining = format!("{} to go", geo::fmt_distance(route.distance_m));
        let rem_top = summary_y + draw::baseline(ctx.text, &duration, &duration_style)
            - draw::baseline(ctx.text, &remaining, &numeric_caption);
        let rem_x = summary.x0 + draw::measure_w(ctx.text, &duration, &duration_style) + s2;
        draw::text(
            &mut ctx,
            &remaining,
            Point::new(rem_x, rem_top),
            &numeric_caption,
            ink.text_dim,
        );
        let end = Rect::new(
            summary.x1 - end_w,
            summary.center().y - end_h / 2.0,
            summary.x1,
            summary.center().y + end_h / 2.0,
        );
        self.navigation_end_rect = end;
        Button::paint(
            ctx.scene,
            visual,
            ctx.text,
            &ink,
            &ButtonFace::new(end, "End")
                .control_size(ControlSize::Small)
                .hovered(self.navigation_end_hovered),
        );
    }

    /// Floating info card for the selected POI, anchored at the bottom of the
    /// visible map area (clear of the docked panel and the control column):
    /// its name, what it is, the facts OpenStreetMap has for it, and
    /// Directions.
    #[allow(
        clippy::many_single_char_names,
        reason = "the shared drawing names (t: transform, x/y: the pen, s1–s4: spacing steps) \
              used consistently across every draw_* method in this file"
    )]
    pub(super) fn draw_poi_card(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let Some(id) = self.selected_poi.clone() else {
            return;
        };
        let Some(poi) = self.pois.iter().find(|p| p.id == id).cloned() else {
            return;
        };
        let ink = on_panel(theme);
        let sp = &theme.tokens.spacing;
        let (s1, s2, s4) = (f64::from(sp.s1), f64::from(sp.s2), f64::from(sp.s4));
        let pad = s4;

        // The facts OpenStreetMap has for this place.
        let mut facts: Vec<PropertyRow<'_>> = Vec::new();
        if !poi.address.is_empty() {
            facts.push(PropertyRow::new("Address", &poi.address));
        }
        if let Some(hours) = &poi.opening_hours {
            facts.push(PropertyRow::new("Hours", hours));
        }
        if let Some(phone) = &poi.phone {
            facts.push(PropertyRow::new("Phone", phone).with_kind(TextKind::Numeric));
        }
        if let Some(web) = &poi.website {
            facts.push(PropertyRow::new("Website", web).with_kind(TextKind::Identifier));
        }
        let properties = PropertyListFace::new(&facts);

        let area = self.visible_map_area();
        let w = f64::from(theme.tokens.metrics.panel_width_list)
            .min((area.width()).max(f64::from(sp.s16) * 3.0));
        let inner_w = w - 2.0 * pad;
        let title = theme.text(TextRole::Title);
        let caption = theme.text(TextRole::Caption);
        let title_h = draw::line_h(tr, &title);
        let caption_h = draw::line_h(tr, &caption);
        let facts_h = if facts.is_empty() {
            0.0
        } else {
            s4 + properties.height(tr, &ink, inner_w)
        };
        let button_h = ButtonFace::height(ControlSize::Regular, theme);
        let card_h = (pad + title_h + s1 + caption_h + facts_h + s4 + button_h + pad).ceil();
        let x0 = (area.center().x - w / 2.0).max(area.x0);
        let y1 = area.y1;
        let card = Rect::new(x0, y1 - card_h, x0 + w, y1);
        self.poi_card_rect = card;
        self.floats(card);
        ui::widget::floating_surface::paint(scene, t, card, theme);
        self.click_targets.push((card, Hit::Consume));

        let x = card.x0 + pad;
        let right = card.x1 - pad;
        let mut y = card.y0 + pad;
        // Close: an icon button seated on the title line.
        let side = ButtonFace::height(ControlSize::Regular, theme);
        let close = Rect::new(
            card.x1 - s2 - side,
            y + (title_h - side) / 2.0,
            card.x1 - s2,
            y + (title_h + side) / 2.0,
        );
        self.icon_button(
            &mut PaintCtx::new(scene, t, tr, &ink),
            close,
            (Icon::X, "Close"),
            ButtonRole::Plain,
            false,
            Hit::ClosePoi,
        );
        let mut ctx = PaintCtx::new(scene, t, tr, &ink);
        draw::text_clip(
            &mut ctx,
            &poi.name,
            Point::new(x, y),
            &title,
            ink.text_primary,
            close.x0 - s2 - x,
        );
        y += title_h + s1;
        // What it is: OpenStreetMap's kind when tagged, else the category.
        draw::text_clip(
            &mut ctx,
            &poi.kind_label(),
            Point::new(x, y),
            &caption,
            ink.text_dim,
            right - x,
        );
        y += caption_h;
        if !facts.is_empty() {
            y += s4;
            let h = properties.height(ctx.text, &ink, inner_w);
            properties.paint(&mut ctx, Rect::new(x, y, right, y + h));
            y += h;
        }
        y += s4;
        let btn = Rect::new(x, y, right, y + button_h);
        self.text_button(
            &mut PaintCtx::new(scene, t, tr, &ink),
            ButtonFace::new(btn, "Directions").role(ButtonRole::Primary),
            btn,
            Some(Hit::PoiDirections),
        );
    }
}
