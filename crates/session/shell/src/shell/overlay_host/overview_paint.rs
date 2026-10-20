//! Overview painting and its panel cache.

#[allow(
    clippy::wildcard_imports,
    reason = "private implementation module shares its enclosing host types"
)]
use super::*;

impl ShellOverlayHost {
    /// The two legacy full-panel modals: the room overview panel and the
    /// workspace-settings sheet. Second of four passes split out of
    /// `draw`.
    pub(super) fn draw_overview_and_ws_settings_overlays(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        flags: crate::shell::chrome_state::OverlayFlags,
    ) {
        let renderer_scale = text_renderer.text_scale();
        text_renderer.set_text_scale(1.0);
        self.draw_overview_overlay(scene, transform, text_renderer, theme, flags.overview);
        self.draw_ws_settings_overlay(scene, transform, text_renderer, theme, flags.ws_settings);
        text_renderer.set_text_scale(renderer_scale);
    }

    /// Shadow falloff plus the rise, so one dirty rectangle covers every
    /// frame of the fade without leaving the previous position behind.
    pub(super) fn overview_shadow_outset(theme: &Theme) -> f64 {
        let token = &theme.tokens.elevation.floating;
        ui::surface::std_dev_for(token.blur) * 2.5 + f64::from(token.offset_y) + 2.0
    }

    pub(super) fn overview_stable_clip(&self, theme: &Theme) -> Rect {
        let outset = Self::overview_shadow_outset(theme) + OVERVIEW_PANEL_RISE_PX;
        let clip = self.overview_panel_rect().inflate(outset, outset);
        let output = self.overview_output_rect();
        let x0 = clip.x0.max(output.x0);
        let y0 = clip.y0.max(output.y0);
        let x1 = clip.x1.min(output.x1);
        let y1 = clip.y1.min(output.y1);
        if x1 <= x0 || y1 <= y0 {
            clip
        } else {
            Rect::new(x0, y0, x1, y1)
        }
    }

    /// The display the overview dims. The combined desktop rect covers every
    /// scanout, and a scrim that wide is the two-output readback.
    pub(super) fn overview_output_rect(&self) -> Rect {
        self.focused_output()
            .or_else(|| self.primary_output())
            .map_or(self.rect, |output| output.rect())
    }

    pub(super) fn overview_panel_cache_key(&self, reduce_motion: bool) -> OverviewPanelCacheKey {
        let panel = self.overview_panel_rect();
        let press = self.overview_press.as_ref().map(|press| {
            (
                press.tab.unwrap_or(0),
                u8::from(press.is_dragging()),
                press.at.x.to_bits(),
                press.at.y.to_bits(),
            )
        });
        OverviewPanelCacheKey {
            scroll_bits: self.overview_scroll_y.to_bits(),
            selection: self.overview_selection,
            window_selection: self.overview_window_selection,
            press,
            panel: (
                panel.x0.to_bits(),
                panel.y0.to_bits(),
                panel.x1.to_bits(),
                panel.y1.to_bits(),
            ),
            chrome_revision: self
                .chrome
                .as_ref()
                .map_or(0, |chrome| chrome.borrow().revision),
            reduce_motion,
        }
    }

    pub(super) fn ensure_overview_panel_cache(
        &mut self,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let key = self.overview_panel_cache_key(theme.reduce_motion);
        if self
            .overview_panel_cache
            .as_ref()
            .is_some_and(|cache| cache.key == key)
        {
            return;
        }
        let mut recorded = Scene::new();
        self.draw_overview_panel(&mut recorded, Affine::IDENTITY, text_renderer, theme);
        self.overview_panel_cache = Some(OverviewPanelCache {
            scene: recorded,
            key,
        });
        self.overview_panel_cache_fills = self.overview_panel_cache_fills.saturating_add(1);
    }

    pub(super) fn paint_cached_overview_panel(
        &mut self,
        scene: &mut Scene,
        layer_transform: Affine,
        panel_xform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        alpha: f32,
    ) {
        self.ensure_overview_panel_cache(text_renderer, theme);
        let Some(cache) = self.overview_panel_cache.as_ref() else {
            return;
        };
        if alpha >= 0.999 {
            scene.append(&cache.scene, Some(panel_xform));
            return;
        }
        let clip = self.overview_stable_clip(theme);
        // The blend is the panel's rectangle, not the combined desktop.
        // A full-viewport layer puts every bin of both outputs into a blend
        // group, and the virtio readback of that frame is what stalls the
        // transition.
        scene.push_layer(
            vello::peniko::Fill::NonZero,
            BlendMode::default(),
            alpha,
            layer_transform,
            &clip,
        );
        scene.append(&cache.scene, Some(panel_xform));
        scene.pop_layer();
    }

    #[cfg(test)]
    pub(crate) fn overview_panel_cache_fills(&self) -> u32 {
        self.overview_panel_cache_fills
    }

    /// The room-overview panel: scrim, scale/translate curve, then the
    /// panel itself. First half of what was
    /// `draw_overview_and_ws_settings_overlays`, split out to keep that
    /// under `too_many_lines`.
    pub(super) fn draw_overview_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        overview_open: bool,
    ) {
        self.reconcile_overview_window_selection();
        let progress =
            self.sync_overview_zoom(overview_open, ui::motion::input_source(), theme.motion());
        // The workspace's windows fly between their strips and the picture
        // (`draw_overview_tiles`) over a ground that hides the desktop they
        // came from; the panel of workspaces arrives with them, the same
        // enter and exit the ws-settings modal plays.
        let (overview_alpha, overview_dy, overview_scale) =
            overview_panel_curve(f32_of_f64(progress));

        // Scale pivot is the panel's centre, so it settles in place
        // wherever the layout put it (beside the picture, or centred).
        let pivot = self.overview_panel_rect().center();
        let (cx, pivot_y) = (pivot.x, pivot.y);
        let scaled = |s: f64| -> Affine {
            Affine::translate((cx, pivot_y))
                .pre_scale(s)
                .pre_translate((-cx, -pivot_y).into())
        };

        if !overview_open {
            // Clear the cached hit rect so a stray click during an
            // in-flight close can't create a workspace. The open's first
            // sample is still alpha 0; that sample must not drop the
            // new-workspace control, which is addressable from geometry
            // for the whole time the overlay flag is up.
            self.add_workspace_rect = Rect::ZERO;
        } else if overview_alpha <= 0.0 {
            self.add_workspace_rect = self.add_workspace_hit_rect();
        }
        // One keyed part, drawn even while it paints nothing, so a fade
        // damages only the display the overview dims: a part that appears
        // or disappears changes the section's structure, which is a dense
        // frame of every display.
        let key = ui::scene_parts::PartKey::new("shell.overview", ());
        ui::scene_parts::part_drawn(scene, key, |scene| {
            if progress <= 0.0 {
                return Vec::new();
            }
            let output = self.overview_output_rect();
            // The overview is the display's: nothing of it (the panel's
            // shadow at the display's edge, a window flying in from the next
            // display) is drawn on another one.
            scene.push_clip_layer(Fill::NonZero, transform, &output);
            ui::overlay::scrim::whole_view(scene, transform, output, f32_of_f64(progress), theme);
            let panel_xform = if (overview_scale - 1.0).abs() > f64::EPSILON
                || overview_dy.abs() > f64::EPSILON
            {
                {
                    // `Affine`'s `Mul` composes transform matrices with plain
                    // float multiply-adds — it cannot overflow or panic —
                    // but kurbo has no non-operator way to compose an
                    // already-built `Affine` (`scaled(...)`) into a chain,
                    // only the canonical-kind `pre_*`/`then_*` combinators.
                    #[allow(
                        clippy::arithmetic_side_effects,
                        reason = "Affine * Affine is a panic-free float matrix multiply; no \
                                  combinator exists to compose an arbitrary already-built Affine"
                    )]
                    {
                        Affine::translate((0.0, overview_dy)) * scaled(overview_scale) * transform
                    }
                }
            } else {
                transform
            };
            self.paint_cached_overview_panel(
                scene,
                transform,
                panel_xform,
                text_renderer,
                theme,
                overview_alpha,
            );
            self.draw_overview_tiles(scene, transform, text_renderer, theme, progress);
            scene.pop_layer();
            vec![transform.transform_rect_bbox(output)]
        });
    }

    /// Paints an overlapping stack of member faces, right-aligned at
    /// `right_x`, centred on `cy`. `ground` must match the surface behind
    /// the stack — the separator ring is a hole punched in the face below.
    /// An absent member stays at reduced opacity rather than disappearing.
    pub(super) fn draw_face_stack(
        ctx: &mut ui::paint::PaintCtx<'_>,
        anchor: Point,
        people: &[tilo_desktop::overview::RoomPerson],
        colors_for: &std::collections::HashMap<String, (u8, u8, u8)>,
        ground: Color,
    ) -> f64 {
        let (right_x, cy) = (anchor.x, anchor.y);
        let r = OVERVIEW_FACE_RADIUS_PX;
        let step = OVERVIEW_FACE_STEP_PX;
        let shown = people.len().min(OVERVIEW_FACES_MAX);
        let overflow = people.len().saturating_sub(shown);
        let slots = shown.saturating_add(usize::from(overflow > 0));
        if slots == 0 {
            return right_x;
        }
        let right = right_x - r;
        for (idx, person) in people.iter().take(shown).enumerate() {
            let cx = right - f64_of_usize(slots.saturating_sub(1).saturating_sub(idx)) * step;
            let (cr, cg, cb) = colors_for
                .get(&person.id)
                .copied()
                .unwrap_or_else(|| ui::presence::user_color(&person.id));
            let fill = Color::from_rgb8(cr, cg, cb);
            // Mixed toward the ground rather than made translucent: the
            // monogram picks its ink from the fill it's drawn on, and on
            // a light panel an alpha-reduced face left white letters on
            // a pale disc.
            let fill = if person.present {
                fill
            } else {
                ui::tokens::colors::mix8(fill, ground, 1.0 - OVERVIEW_FACE_AWAY_ALPHA)
            };
            let initial = person
                .name
                .chars()
                .next()
                .unwrap_or('?')
                .to_ascii_uppercase()
                .to_string();
            ui::widget::AvatarFace::new(&initial, "")
                .presence(fill)
                .ringed_on(ground)
                .paint(ctx, Rect::new(cx - r, cy - r, cx + r, cy + r));
        }
        if overflow > 0 {
            let cx = right;
            ui::widget::AvatarFace::paint_overflow(
                ctx,
                Rect::new(cx - r, cy - r, cx + r, cy + r),
                overflow,
                Some(ground),
            );
        }
        right - f64_of_usize(slots.saturating_sub(1)) * step - r
    }

    /// Per-identity avatar colours for one room, matching that person's
    /// cursor and top-bar chip.
    pub(super) fn overview_face_colors(
        &self,
        idx: usize,
    ) -> std::collections::HashMap<String, (u8, u8, u8)> {
        match self.chrome.as_ref() {
            Some(c) => {
                let state = c.borrow();
                let tree = state.tree();
                match tree.desktop.workspaces.get(idx) {
                    // Chrome already holds the session identity. Do not
                    // call `local_user_id()` here: latching the overview
                    // during update means the first paint can draw the
                    // panel, and fixtures forbid that ambient start.
                    Some(ws) => tilo_desktop::collaborators::roster_colors_with_local_user(
                        &ws.collaborators,
                        &state.local_user_id,
                    ),
                    None => std::collections::HashMap::default(),
                }
            }
            None => std::collections::HashMap::default(),
        }
    }

    /// One room row in the "go to" list; `trailing` is kept free at its end
    /// for a control of the row's own (a site workspace's Leave).
    pub(super) fn draw_overview_room_row(
        &self,
        ctx: &mut ui::paint::PaintCtx<'_>,
        rect: Rect,
        idx: usize,
        card: &tilo_desktop::overview::RoomCard,
        selected: bool,
        trailing: f64,
    ) {
        let (scene, transform, text_renderer, theme) =
            (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let ground = ui::chrome::shell::panel_ground(theme);
        let hover = self.last_cursor.is_some_and(|c| rect.contains(c));
        let m = self.overview_metrics;

        let rr = RoundedRect::from_rect(rect, m.row_radius);
        // A room row is a Control — a list row you press — so "which one is
        // chosen" is the shared selection tier (HIG §9.3), not an accent
        // tint: idle transparent, hover `lift(0.5)`, selected `lift(1.0)`
        // with the hairline edge and inner glow every other active control
        // carries. Until 2026-09-17 the selected row wore a 16 % accent
        // fill and an accent stroke (§9.4 reserves accent for live state)
        // and the idle row a `neutral[1]` ramp slot (§9.2; S1-27).
        ui::chrome::selection_tier(
            theme,
            ui::chrome::SelectionState::new(selected, hover, false),
        )
        .paint(scene, transform, rect, m.row_radius);
        if !selected && !hover {
            // A destination occupies the whole row, not only its label and
            // avatar: a derived hairline keeps the click target legible on
            // the panel ground without giving it a body of its own.
            scene.stroke(
                &vello::kurbo::Stroke::new(1.0),
                transform,
                ui::tokens::colors::Colors::hairline_on(ground, ui::chrome::shell::HAIRLINE_ALPHA),
                None,
                &rr,
            );
        }

        let pad = m.row_pad;
        let x0 = rect.x0 + pad;
        let x1 = rect.x1 - pad - trailing;

        // Faces first: the name elides against what's left rather than
        // running underneath them.
        let colors_for = self.overview_face_colors(idx);
        let faces_left = Self::draw_face_stack(
            &mut ui::paint::PaintCtx::new(scene, transform, text_renderer, theme),
            Point::new(x1, rect.center().y),
            &card.people,
            &colors_for,
            ground,
        );
        let s3 = f64::from(tokens.spacing.s3);
        let anchor = (faces_left - s3, rect.center().y);
        let marks_left =
            Self::draw_overview_app_marks(scene, transform, theme, anchor, &card.apps());
        let text_w = (marks_left - s3 - x0).max(40.0);

        let name_style = tokens.typography.style(TextRole::Heading);
        let name = text_renderer.elide_to_width_style(&card.name, f32_of_f64(text_w), &name_style);
        let (_, name_h) = text_renderer.measure_style(&name, None, &name_style);
        let sub_style = tokens.typography.style(TextRole::Caption);
        // Who's in the room now outranks who could be; access is what
        // the line says when it's empty.
        let mut sub = self.overview_site_line(idx).unwrap_or_else(|| {
            tilo_desktop::overview::room_presence_label(card)
                .unwrap_or_else(|| tilo_desktop::overview::room_access_label(card))
        });
        if let Some(shares) = tilo_desktop::overview::room_shares_label(card) {
            sub = format!("{sub} · {shares}");
        }
        let sub = text_renderer.elide_to_width_style(&sub, f32_of_f64(text_w), &sub_style);
        let (_, sub_h) = text_renderer.measure_style(&sub, None, &sub_style);
        // Name and sub-line are one stack, `s1` apart, centred on the row.
        let line_gap = f64::from(tokens.spacing.s1);
        let stack_h = f64::from(name_h) + line_gap + f64::from(sub_h);
        let top = rect.center().y - stack_h / 2.0;
        text_renderer.draw_style(
            scene,
            &name,
            colors.text_primary,
            transform.pre_translate((x0, top).into()),
            None,
            &name_style,
        );
        text_renderer.draw_style(
            scene,
            &sub,
            colors.text_dim,
            transform.pre_translate((x0, top + f64::from(name_h) + line_gap).into()),
            None,
            &sub_style,
        );
        if self.overview_selection == Some(idx) && self.overview_window_selection.is_none() {
            ui::widget::paint_focus_ring(
                scene,
                transform,
                rect.inset(-2.0),
                (m.row_radius - 2.0).max(0.0),
                theme,
            );
        }
    }

    /// Marks of the apps open in a room, ending at `right` and centred on
    /// `center_y`, so a room is recognisable by what is in it. Returns the
    /// left edge of what was drawn (`right` when the room is empty).
    pub(super) fn draw_overview_app_marks(
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        (right, center_y): (f64, f64),
        apps: &[&tilo_desktop::window_switcher::WindowApp],
    ) -> f64 {
        /// More than this reads as a pile rather than a list of apps.
        const MOST: usize = 5;
        let edge = f64::from(theme.tokens.metrics.icon_md);
        let gap = f64::from(theme.tokens.spacing.s1);
        let mut left = right;
        for app in apps.iter().take(MOST).rev() {
            let mark = Rect::new(
                left - edge,
                center_y - edge / 2.0,
                left,
                center_y + edge / 2.0,
            );
            ui::widget::draw_app_mark(
                scene,
                transform,
                mark,
                app.icon.as_ref(),
                app.kind,
                theme.tokens.colors.text_dim,
            );
            left = mark.x0 - gap;
        }
        if left < right { left + gap } else { right }
    }

    /// The whole overview at rest: the panel of workspaces and the picture
    /// of this workspace's strips and windows, as a settled frame shows it.
    #[cfg(test)]
    pub(super) fn draw_overview(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        self.draw_overview_panel(scene, transform, text_renderer, theme);
        self.draw_overview_tiles(scene, transform, text_renderer, theme, 1.0);
    }

    /// The panel of workspaces: a floating panel like the palette and the
    /// workspace-settings modal, beside the picture of this workspace on a
    /// wide display and holding it on a constrained one, where it fills the
    /// window as a compact page. Reading order follows: where you are,
    /// what's shared, where else to go. Recorded once and reused while it
    /// shows the same thing; the windows are drawn over it, per window.
    pub(super) fn draw_overview_panel(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        if self.chrome.is_none() {
            return;
        }
        self.resolve_overview_layout(theme);
        let cards = self.overview_cards();
        let tokens = &theme.tokens;
        let colors = &tokens.colors;

        // Same fill/hairline/lift as the workspace-settings modal: one
        // floating-surface language across the shell's overlays.
        let panel = self.overview_panel_rect();
        let m = self.overview_metrics;
        self.draw_overview_backdrop(scene, transform, theme, panel);

        let content = self.overview_content_rect();
        let here_idx = self.overview_here_index();
        let here_rect = self.overview_here_rect();
        let caption_style = tokens.typography.style(TextRole::Caption);

        let whole = self.overview_scrolls_whole_panel();
        if whole {
            scene.push_layer(
                Fill::NonZero,
                vello::peniko::BlendMode::default(),
                1.0,
                transform,
                &Rect::new(panel.x0, content.y0, panel.x1, content.y1),
            );
        }
        self.draw_overview_here_section(
            &mut ui::paint::PaintCtx::new(scene, transform, text_renderer, theme),
            content,
            here_rect,
            here_idx,
            &cards,
        );
        self.draw_overview_shares_section(
            scene,
            transform,
            text_renderer,
            theme,
            content,
            here_idx,
            &cards,
        );

        // -- Where else you can go ----------------------------------------
        text_renderer.draw_style(
            scene,
            &t("overview-go-to"),
            colors.text_tertiary,
            transform.pre_translate(
                (
                    content.x0,
                    self.overview_list_caption_top() + m.caption_text_dy,
                )
                    .into(),
            ),
            None,
            &caption_style,
        );

        let band = Rect::new(
            content.x0 - 2.0,
            self.overview_list_top().max(content.y0),
            content.x1 + 2.0,
            content.y1,
        );
        scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &band,
        );

        self.draw_overview_go_to_list(scene, transform, text_renderer, theme, &cards, band);
        self.draw_overview_add_room_row(scene, transform, text_renderer, theme, cards.len());
        self.draw_overview_join_room_row(scene, transform, text_renderer, theme, cards.len());
        self.draw_overview_scroll_fade(scene, transform, theme, if whole { content } else { band });

        scene.pop_layer();
        if whole {
            scene.pop_layer();
        }
    }

    pub(super) fn draw_overview_backdrop(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        panel: Rect,
    ) {
        let m = self.overview_metrics;
        let ground = ui::chrome::shell::panel_ground(theme);
        // A Panel (HIG §9.1): `radii.panel`, and a shadow that follows its own
        // rounded outline rather than a square field.
        if self.overview_uses_full_window() {
            ui::floating_surface::paint_material(
                scene,
                transform,
                &panel,
                ui::floating_surface::Material::Opaque,
                theme,
            );
        } else {
            ui::surface::shadow(
                scene,
                transform,
                panel,
                m.panel_radius,
                &theme.tokens.elevation.floating,
            );
            let card_rr = RoundedRect::from_rect(panel, m.panel_radius);
            scene.fill(Fill::NonZero, transform, ground, None, &card_rr);
            // A Panel's edge is derived (HIG §9.5), never `border_standard`.
            scene.stroke(
                &vello::kurbo::Stroke::new(1.0),
                transform,
                ui::chrome::shell::panel_border(theme),
                None,
                &ui::surface::edge_outline(panel, m.panel_radius),
            );
        }
    }

    /// "Where you are": the current room's face stack, name, "You are
    /// here" chip and access label — or an honest empty state when
    /// nothing is marked current.
    pub(super) fn draw_overview_here_section(
        &self,
        ctx: &mut ui::paint::PaintCtx<'_>,
        content: Rect,
        here_rect: Rect,
        here_idx: Option<usize>,
        cards: &[tilo_desktop::overview::RoomCard],
    ) {
        let (scene, transform, text_renderer, theme) =
            (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let name_size = Self::overview_header_font_size_for(tokens);

        if let Some((hi, card)) = here_idx.and_then(|hi| cards.get(hi).map(|card| (hi, card))) {
            self.draw_overview_here_room(
                &mut ui::paint::PaintCtx::new(scene, transform, text_renderer, theme),
                content,
                here_rect,
                hi,
                card,
                name_size,
            );
            let leave = self.overview_leave_rect(hi, &self.site_rooms());
            if leave.area() > 0.0 {
                Button::paint(
                    scene,
                    transform,
                    text_renderer,
                    theme,
                    &ButtonFace::new(leave, &t("overview-leave-workspace"))
                        .hovered(self.last_cursor.is_some_and(|p| leave.contains(p))),
                );
            }
        } else {
            // No room is marked current. Saying which one you are in
            // would be an invention, so the panel says what is true.
            text_renderer.draw_text_weighted(
                scene,
                &t("overview-no-room"),
                name_size,
                colors.text_dim,
                transform.pre_translate((content.x0, here_rect.y0).into()),
                None,
                TextWeight::Bold,
            );
        }
    }

    /// The current room's selection highlight, face stack, name, "You
    /// are here" chip and access label.
    pub(super) fn draw_overview_here_room(
        &self,
        ctx: &mut ui::paint::PaintCtx<'_>,
        content: Rect,
        here_rect: Rect,
        hi: usize,
        card: &tilo_desktop::overview::RoomCard,
        name_size: f32,
    ) {
        use ui::widget::{ChipFace, ChipLead, ChipStyle};

        let (scene, transform, text_renderer, theme) =
            (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);

        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let ground = ui::chrome::shell::panel_ground(theme);
        let m = self.overview_metrics;
        let s3 = f64::from(tokens.spacing.s3);
        let s4 = f64::from(tokens.spacing.s4);

        self.draw_overview_here_selection(scene, transform, theme, content, here_rect, hi, ground);

        // Faces claim the right edge, centred on the name line; the name
        // is elided against the room they leave rather than sliding under
        // them.
        let colors_for = self.overview_face_colors(hi);
        let faces_left = Self::draw_face_stack(
            &mut ui::paint::PaintCtx::new(scene, transform, text_renderer, theme),
            Point::new(content.x1, here_rect.y0 + m.name_lh / 2.0),
            &card.people,
            &colors_for,
            ground,
        );

        // "You are here" states which of these rooms you are in: a quiet
        // neutral chip. Your colour is for the focus ring, the caret and
        // your presence among others (HIG 9.4), not for a label.
        let chip_h = m.chip_h;
        let here_label = t("overview-you-are-here");
        let chip_w =
            ChipFace::intrinsic_width(&here_label, ChipLead::None, false, text_renderer, theme);
        let anchor = (faces_left - s4, here_rect.y0 + m.name_lh / 2.0);
        let marks_left =
            Self::draw_overview_app_marks(scene, transform, theme, anchor, &card.apps());
        let name_avail = (marks_left - s4 - content.x0 - chip_w - s3).max(60.0);
        let name = text_renderer.elide_to_width_weighted(
            &card.name,
            name_size,
            f32_of_f64(name_avail),
            TextWeight::Bold,
        );
        let (name_w, name_h) =
            text_renderer.measure_weighted(&name, name_size, None, TextWeight::Bold);
        text_renderer.draw_text_weighted(
            scene,
            &name,
            name_size,
            colors.text_primary,
            transform.pre_translate((content.x0, here_rect.y0).into()),
            None,
            TextWeight::Bold,
        );
        let chip_x0 = content.x0 + f64::from(name_w) + s3;
        ChipFace::new(
            Rect::new(
                chip_x0,
                here_rect.y0 + f64::from(name_h) / 2.0 - chip_h / 2.0,
                chip_x0 + chip_w,
                here_rect.y0 + f64::from(name_h) / 2.0 + chip_h / 2.0,
            ),
            &here_label,
        )
        .style(ChipStyle::Neutral)
        .paint(scene, transform, text_renderer, theme);

        let access = self
            .overview_site_line(hi)
            .unwrap_or_else(|| tilo_desktop::overview::room_access_label(card));
        text_renderer.draw_style(
            scene,
            &access,
            colors.text_dim,
            transform.pre_translate(
                (
                    content.x0,
                    here_rect.y0 + f64::from(name_h) + f64::from(tokens.spacing.s1),
                )
                    .into(),
            ),
            None,
            &tokens.typography.style(TextRole::Body),
        );
    }

    /// The keyboard target lands here on open, so the current room
    /// carries the same accent selection a list row does — without it
    /// the first arrow press would seem to come from nowhere.
    pub(super) fn draw_overview_here_selection(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        content: Rect,
        here_rect: Rect,
        hi: usize,
        ground: Color,
    ) {
        if self.overview_selection != Some(hi) {
            return;
        }
        // The frame stands `s2` off the block's text on three sides and
        // stops short of the block's bottom air, so it does not run into
        // the caption beneath.
        let s2 = f64::from(theme.tokens.spacing.s2);
        let sel = Rect::new(
            content.x0 - s2,
            here_rect.y0 - s2,
            content.x1 + s2,
            here_rect.y1 - s2,
        );
        // The keyboard's choice is a lift of the panel, with its edge.
        let sel_rr = RoundedRect::from_rect(sel, self.overview_metrics.row_radius);
        let fill = theme.tokens.colors.lift_on(ground, 1.0);
        scene.fill(Fill::NonZero, transform, fill, None, &sel_rr);
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            ui::surface::edge_on(fill),
            None,
            &sel_rr,
        );
        if self.overview_window_selection.is_none() {
            let focus = if self.overview_scrolls_whole_panel() {
                sel.intersect(content)
            } else {
                sel
            };
            ui::widget::paint_focus_ring(
                scene,
                transform,
                focus.inset(-2.0),
                (self.overview_metrics.row_radius - 2.0).max(0.0),
                theme,
            );
        }
    }

    /// "What leaves this room": the share list. Nothing at all when the
    /// room shares nothing.
    pub(super) fn draw_overview_shares_section(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        content: Rect,
        here_idx: Option<usize>,
        cards: &[tilo_desktop::overview::RoomCard],
    ) {
        let Some(here_card) = here_idx
            .and_then(|i| cards.get(i))
            .filter(|card| !card.shares.is_empty())
        else {
            return;
        };
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let caption_style = tokens.typography.style(TextRole::Caption);

        let m = self.overview_metrics;
        let shares_top = self.overview_shares_top();
        text_renderer.draw_style(
            scene,
            &t("overview-shared-out"),
            colors.text_tertiary,
            transform.pre_translate((content.x0, shares_top + m.caption_text_dy).into()),
            None,
            &caption_style,
        );
        Self::draw_overview_share_rows(
            scene,
            transform,
            text_renderer,
            theme,
            content,
            here_card,
            shares_top + m.caption_h,
            m.share_row_h,
        );
    }

    /// One row per share (up to `OVERVIEW_SHARES_MAX_ROWS`), plus an
    /// "and N more" trailer when there are more than that.
    pub(super) fn draw_overview_share_rows(
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        content: Rect,
        here_card: &tilo_desktop::overview::RoomCard,
        mut sy: f64,
        row_h: f64,
    ) {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        // One vertical rule for the section: glyph and text centre on
        // the row's middle line.
        let glyph = 14.0_f64;
        let shown = here_card.shares.len().min(OVERVIEW_SHARES_MAX_ROWS);
        for share in here_card.shares.iter().take(shown) {
            Self::draw_overview_share_row(
                &mut ui::paint::PaintCtx::new(scene, transform, text_renderer, theme),
                Rect::new(content.x0, sy, content.x1, sy + row_h),
                share,
                glyph,
            );
            sy += row_h;
        }
        let hidden = here_card.shares.len().saturating_sub(shown);
        if hidden > 0 {
            let more = t_args("overview-more-shares", &[("count", hidden.into())]);
            let (_, more_h) = text_renderer.measure_style(
                &more,
                None,
                &tokens.typography.style(TextRole::Caption),
            );
            text_renderer.draw_style(
                scene,
                &more,
                colors.text_tertiary,
                transform.pre_translate(
                    (
                        content.x0 + glyph + f64::from(tokens.spacing.s2),
                        sy + (row_h - f64::from(more_h)) / 2.0,
                    )
                        .into(),
                ),
                None,
                &tokens.typography.style(TextRole::Caption),
            );
        }
    }

    /// One "shared out of here" row: an icon, who holds it, and what.
    pub(super) fn draw_overview_share_row(
        ctx: &mut ui::paint::PaintCtx<'_>,
        row: Rect,
        share: &tilo_desktop::overview::RoomShare,
        glyph: f64,
    ) {
        let (scene, transform, text_renderer, theme) =
            (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
        let (content, sy, row_h) = (row, row.y0, row.height());
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let body_style = tokens.typography.style(TextRole::Body);
        let (_, body_h) = text_renderer.measure_style("Ag", None, &body_style);
        let body_top = sy + (row_h - f64::from(body_h)) / 2.0;
        let cy = sy + row_h / 2.0;
        let glyph_rect = Rect::new(
            content.x0,
            cy - glyph / 2.0,
            content.x0 + glyph,
            cy + glyph / 2.0,
        );
        ui::icons::draw_icon(
            scene,
            transform,
            glyph_rect,
            ui::icons::Icon::ExternalLink,
            colors.text_tertiary,
            ui::icons::LUCIDE_DEFAULT_STROKE_PX,
        );
        // Who holds it sits right; what's shared elides against it.
        let with = match (share.with.first(), share.with.len()) {
            (Some(first), 1) => first.clone(),
            (Some(first), 2) => t_args(
                "overview-share-with-two",
                &[
                    ("first", first.as_str().into()),
                    (
                        "second",
                        share.with.get(1).map_or("", String::as_str).into(),
                    ),
                ],
            ),
            (Some(first), n) => t_args(
                "overview-share-with-many",
                &[
                    ("first", first.as_str().into()),
                    ("count", n.saturating_sub(1).into()),
                ],
            ),
            (None, _) => String::new(),
        };
        let with = if share.writable {
            t_args("overview-share-can-edit", &[("with", with.into())])
        } else {
            with
        };
        let with_style = tokens.typography.style(TextRole::Caption);
        let (with_w, with_h) = text_renderer.measure_style(&with, None, &with_style);
        text_renderer.draw_style(
            scene,
            &with,
            colors.text_dim,
            transform.pre_translate(
                (
                    content.x1 - f64::from(with_w),
                    sy + (row_h - f64::from(with_h)) / 2.0,
                )
                    .into(),
            ),
            None,
            &with_style,
        );
        let what_x = content.x0 + glyph + f64::from(tokens.spacing.s2);
        let what_avail =
            (content.x1 - f64::from(with_w) - f64::from(tokens.spacing.s3) - what_x).max(40.0);
        let what =
            text_renderer.elide_to_width_style(&share.what, f32_of_f64(what_avail), &body_style);
        text_renderer.draw_style(
            scene,
            &what,
            colors.text_primary,
            transform.pre_translate((what_x, body_top).into()),
            None,
            &body_style,
        );
    }

    /// "Where else you can go": one row per room, clipped to the
    /// scrollable band so off-screen rows are skipped rather than drawn.
    pub(super) fn draw_overview_go_to_list(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        cards: &[tilo_desktop::overview::RoomCard],
        band: Rect,
    ) {
        // A carried window shows the room it would land in as chosen.
        let drop_target = self.overview_drop_target();
        let site_rooms = self.site_rooms();
        for (idx, card) in cards.iter().enumerate() {
            let Some(_slot) = self.overview_slot_of(idx) else {
                continue;
            };
            let rect = self.overview_row_rect(idx);
            if rect.y1 < band.y0 || rect.y0 > band.y1 {
                continue;
            }
            let leave = self.overview_leave_rect(idx, &site_rooms);
            let trailing = if leave.area() > 0.0 {
                leave.width() + f64::from(theme.tokens.spacing.s3)
            } else {
                0.0
            };
            self.draw_overview_room_row(
                &mut ui::paint::PaintCtx::new(scene, transform, text_renderer, theme),
                rect,
                idx,
                card,
                self.overview_selection == Some(idx) || drop_target == Some(idx),
                trailing,
            );
            if leave.area() > 0.0 {
                Button::paint(
                    scene,
                    transform,
                    text_renderer,
                    theme,
                    &ButtonFace::new(leave, &t("overview-leave-workspace"))
                        .hovered(self.last_cursor.is_some_and(|p| leave.contains(p))),
                );
            }
        }
    }

    /// A peer row in the same list, not a banner: creating a room is
    /// one of the things you can go and do from here.
    pub(super) fn draw_overview_add_room_row(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        cards_len: usize,
    ) {
        use ui::widget::{Interaction, ListRowFace, RowLeading};
        let add_rect = self.overview_add_workspace_rect_for(cards_len);
        let focused =
            self.overview_selection == Some(cards_len) && self.overview_window_selection.is_none();
        let label = t("overview-new-workspace");
        ListRowFace::new(&label)
            .leading(RowLeading::Icon(ui::icons::Icon::Plus))
            .selected(focused)
            .interaction(Interaction {
                hovered: self
                    .last_cursor
                    .is_some_and(|cursor| add_rect.contains(cursor)),
                focused,
                ..Interaction::default()
            })
            .paint(
                &mut ui::paint::PaintCtx::new(scene, transform, text_renderer, theme),
                add_rect,
            );
        self.add_workspace_rect = add_rect;
    }

    /// "Join a workspace…": the one field a site, a workspace link or an
    /// invitation is joined through, beside creating one.
    pub(super) fn draw_overview_join_room_row(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        cards_len: usize,
    ) {
        use ui::widget::{Interaction, ListRowFace, RowLeading};
        let rect = self.overview_join_workspace_rect_for(cards_len);
        let focused = self.overview_selection == Some(cards_len.saturating_add(1))
            && self.overview_window_selection.is_none();
        let label = t("overview-join-workspace");
        ListRowFace::new(&label)
            .leading(RowLeading::Icon(ui::icons::Icon::Link))
            .selected(focused)
            .interaction(Interaction {
                hovered: self.last_cursor.is_some_and(|cursor| rect.contains(cursor)),
                focused,
                ..Interaction::default()
            })
            .paint(
                &mut ui::paint::PaintCtx::new(scene, transform, text_renderer, theme),
                rect,
            );
    }

    /// Overflow cue: a bottom fade over the scrollable band, only when
    /// there is more to scroll to.
    pub(super) fn draw_overview_scroll_fade(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        band: Rect,
    ) {
        if self.overview_scroll_y + 0.5 < self.overview_max_scroll_y() {
            let fade_y1 = band.y1;
            let fade_y0 = fade_y1 - OVERVIEW_SCROLL_FADE_PX;
            let ground = ui::chrome::shell::panel_ground(theme);
            let grad = vello::peniko::Gradient::new_linear(
                Point::new(band.x0, fade_y0),
                Point::new(band.x0, fade_y1),
            )
            .with_extend(vello::peniko::Extend::Pad)
            .with_stops(
                &[
                    vello::peniko::ColorStop {
                        offset: 0.0,
                        color: ground.with_alpha_factor(0.0).into(),
                    },
                    vello::peniko::ColorStop {
                        offset: 1.0,
                        color: ground.into(),
                    },
                ][..],
            );
            scene.fill(
                Fill::NonZero,
                transform,
                &grad,
                None,
                &Rect::new(band.x0, fade_y0, band.x1, fade_y1),
            );
        }
    }
}
