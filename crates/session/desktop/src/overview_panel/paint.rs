//! Painting the overview: the panel of workspaces (recorded once and kept
//! while it shows the same thing), and the picture's names, titles and
//! selection over it. The windows' pictures themselves are the host's to
//! draw where it holds them ([`crate::window_flight`]); a host that holds
//! none gets each window's app mark here.

use std::collections::HashMap;

use ui::TextRole;
use ui::num::{f32_of_f64, f64_of_usize};
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::{Button, ButtonFace, ControlSize, FieldFace, IconButton, TextInput, Widget as _};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::{BlendMode, Color, Fill};

use super::OverviewPanel;
use super::layout::{Picture, SHARES_MAX_ROWS};
use crate::i18n::{t, t_args};
use crate::overview::{RoomCard, RoomPerson, RoomShare};
use crate::overview_picture::{Tile, draw_empty_strip, draw_strip_name, draw_tile_title};
use crate::window_flight::WindowPicture;
use crate::window_switcher::{NoThumbnails, PreviewWell, WindowApp, draw_preview_well};

/// A pictured window carried to another workspace is left this faint in
/// its place.
pub(super) const CARRIED_ALPHA: f32 = 0.35;

/// How much of their own colour an away member's face keeps, the rest being
/// the panel behind it.
const FACE_AWAY_ALPHA: f32 = 0.55;

/// Most faces drawn before the rest collapse into a "+N" disc.
const FACES_MAX: usize = 4;

impl OverviewPanel {
    /// Shadow falloff plus the rise, so one clip covers every frame of the
    /// panel's fade without leaving its last place behind.
    fn shadow_outset(theme: &Theme) -> f64 {
        let token = &theme.tokens.elevation.floating;
        ui::surface::std_dev_for(token.blur) * 2.5 + f64::from(token.offset_y) + 2.0
    }

    fn stable_clip(&self, theme: &Theme) -> Rect {
        let outset = Self::shadow_outset(theme) + super::layout::PANEL_RISE_PX;
        let clip = self.panel_rect().inflate(outset, outset);
        let shown = clip.intersect(self.rect);
        if shown.width() <= 0.0 || shown.height() <= 0.0 {
            clip
        } else {
            shown
        }
    }

    /// The panel of workspaces, at `(alpha, dy, scale)` of its arrival:
    /// recorded once and replayed while it shows the same thing.
    pub(super) fn paint_panel_at(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        (alpha, dy, scale): (f32, f64, f64),
    ) {
        if alpha <= 0.0 {
            return;
        }
        if self
            .cache
            .as_ref()
            .is_none_or(|(revision, _)| *revision != self.revision)
        {
            let mut recorded = Scene::new();
            self.paint_panel(&mut recorded, Affine::IDENTITY, text, theme);
            self.cache = Some((self.revision, recorded));
            self.cache_fills = self.cache_fills.saturating_add(1);
        }
        let Some((_, recorded)) = self.cache.as_ref() else {
            return;
        };
        // The scale's pivot is the panel's centre, so it settles in place
        // wherever the layout put it.
        let pivot = self.panel_rect().center();
        let placed = if (scale - 1.0).abs() > f64::EPSILON || dy.abs() > f64::EPSILON {
            let scaled = Affine::translate((pivot.x, pivot.y + dy))
                .pre_scale(scale)
                .pre_translate((-pivot.x, -pivot.y).into());
            #[allow(
                clippy::arithmetic_side_effects,
                reason = "Affine * Affine is a panic-free float matrix multiply"
            )]
            {
                transform * scaled
            }
        } else {
            transform
        };
        if alpha >= 0.999 {
            scene.append(recorded, Some(placed));
            return;
        }
        // The blend is the panel's own rectangle, never the whole display.
        let clip = self.stable_clip(theme);
        scene.push_layer(Fill::NonZero, BlendMode::default(), alpha, transform, &clip);
        scene.append(recorded, Some(placed));
        scene.pop_layer();
    }

    /// The panel of workspaces: where you are, what is shared out of it,
    /// where else to go.
    fn paint_panel(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
    ) {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let panel = self.panel_rect();
        let m = self.metrics;
        self.paint_backdrop(scene, transform, theme, panel);

        let content = self.content_rect();
        let here_rect = self.here_rect();
        let whole = self.scrolls_whole_panel();
        if whole {
            scene.push_layer(
                Fill::NonZero,
                BlendMode::default(),
                1.0,
                transform,
                &Rect::new(panel.x0, content.y0, panel.x1, content.y1),
            );
        }
        self.paint_here_section(
            &mut ui::paint::PaintCtx::new(scene, transform, text, theme),
            content,
            here_rect,
        );
        self.paint_shares_section(
            &mut ui::paint::PaintCtx::new(scene, transform, text, theme),
            content,
        );
        let caption = tokens.typography.style(TextRole::Caption);
        text.draw_style(
            scene,
            &t("overview-go-to"),
            colors.text_tertiary,
            transform
                .pre_translate((content.x0, self.list_caption_top() + m.caption_text_dy).into()),
            None,
            &caption,
        );
        let band = Rect::new(
            content.x0 - 2.0,
            self.list_top().max(content.y0),
            content.x1 + 2.0,
            content.y1,
        );
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, &band);
        self.paint_go_to_list(
            &mut ui::paint::PaintCtx::new(scene, transform, text, theme),
            band,
        );
        self.paint_list_row(
            &mut ui::paint::PaintCtx::new(scene, transform, text, theme),
            self.add_rect(),
            self.desk.cards.len(),
            (&t("overview-new-workspace"), ui::icons::Icon::Plus),
        );
        self.paint_list_row(
            &mut ui::paint::PaintCtx::new(scene, transform, text, theme),
            self.join_rect(),
            self.desk.cards.len().saturating_add(1),
            (&t("overview-join-workspace"), ui::icons::Icon::Link),
        );
        if self.scroll_y + 0.5 < self.max_scroll_y() {
            ui::surface::content_edge_fade(
                scene,
                transform,
                if whole { content } else { band },
                ui::surface::ContentEdge::Bottom,
                ui::chrome::shell::panel_ground(theme),
            );
        }
        scene.pop_layer();
        if whole {
            scene.pop_layer();
        }
    }

    fn paint_backdrop(&self, scene: &mut Scene, transform: Affine, theme: &Theme, panel: Rect) {
        let m = self.metrics;
        if self.uses_full_window() {
            ui::floating_surface::paint_material(
                scene,
                transform,
                &panel,
                ui::floating_surface::Material::Opaque,
                theme,
            );
            return;
        }
        ui::surface::shadow(
            scene,
            transform,
            panel,
            m.panel_radius,
            &theme.tokens.elevation.floating,
        );
        let shape = RoundedRect::from_rect(panel, m.panel_radius);
        ui::paint::fill_shape(
            scene,
            transform,
            &shape,
            ui::chrome::shell::panel_ground(theme),
        );
        ui::chrome::shell::paint_panel_edge(scene, transform, panel, m.panel_radius, theme);
    }

    /// "Where you are": the current room's faces, name, "You are here" chip
    /// and access line, or a plain statement that no room is marked current.
    fn paint_here_section(
        &self,
        ctx: &mut ui::paint::PaintCtx<'_>,
        content: Rect,
        here_rect: Rect,
    ) {
        let here = self
            .here_index()
            .and_then(|hi| self.desk.cards.get(hi).map(|card| (hi, card)));
        let Some((hi, card)) = here else {
            let theme = ctx.theme;
            ctx.text.draw_style(
                ctx.scene,
                &t("overview-no-room"),
                theme.tokens.colors.text_dim,
                ctx.transform
                    .pre_translate((content.x0, here_rect.y0).into()),
                None,
                &theme.text(TextRole::Display).with_weight(TextWeight::Bold),
            );
            return;
        };
        self.paint_here_room(ctx, content, here_rect, hi, card);
        if let Some(leave) = self.leave_rect(hi) {
            self.paint_leave(ctx, leave);
        }
    }

    fn paint_leave(&self, ctx: &mut ui::paint::PaintCtx<'_>, leave: Rect) {
        Button::paint(
            ctx.scene,
            ctx.transform,
            ctx.text,
            ctx.theme,
            &ButtonFace::new(leave, &t("overview-leave-workspace"))
                .hovered(self.cursor.is_some_and(|p| leave.contains(p))),
        );
    }

    fn paint_here_room(
        &self,
        ctx: &mut ui::paint::PaintCtx<'_>,
        content: Rect,
        here_rect: Rect,
        hi: usize,
        card: &RoomCard,
    ) {
        use ui::widget::{ChipFace, ChipLead, ChipStyle};
        let (scene, transform, text, theme) =
            (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let ground = ui::chrome::shell::panel_ground(theme);
        let m = self.metrics;

        self.paint_here_selection(scene, transform, theme, content, here_rect, hi);

        // Faces claim the right edge, centred on the name line; the name
        // elides against what they leave.
        let faces_left = paint_face_stack(
            &mut ui::paint::PaintCtx::new(scene, transform, text, theme),
            Point::new(content.x1, here_rect.y0 + m.name_lh / 2.0),
            &card.people,
            self.colors(),
            ground,
        );
        // "You are here" says which of these rooms you are in: a quiet
        // neutral chip. Your colour is for your focus and presence.
        let here_label = t("overview-you-are-here");
        let chip_w = ChipFace::intrinsic_width(&here_label, ChipLead::None, false, text, theme);
        let anchor = (faces_left - m.s4, here_rect.y0 + m.name_lh / 2.0);
        let marks_left = paint_app_marks(scene, transform, theme, anchor, &card.apps());
        let name_avail = (marks_left - m.s4 - content.x0 - chip_w - m.s3).max(60.0);
        let name_style = theme.text(TextRole::Display).with_weight(TextWeight::Bold);
        let name = text.elide_to_width_style(&card.name, f32_of_f64(name_avail), &name_style);
        let (name_w, name_h) = text.measure_style(&name, None, &name_style);
        text.draw_style(
            scene,
            &name,
            colors.text_primary,
            transform.pre_translate((content.x0, here_rect.y0).into()),
            None,
            &name_style,
        );
        let chip_x0 = content.x0 + f64::from(name_w) + m.s3;
        let mid = here_rect.y0 + f64::from(name_h) / 2.0;
        ChipFace::new(
            Rect::new(
                chip_x0,
                mid - m.chip_h / 2.0,
                chip_x0 + chip_w,
                mid + m.chip_h / 2.0,
            ),
            &here_label,
        )
        .style(ChipStyle::Neutral)
        .paint(scene, transform, text, theme);

        let access = self
            .desk
            .sites
            .get(&card.id)
            .cloned()
            .unwrap_or_else(|| crate::overview::room_access_label(card));
        text.draw_style(
            scene,
            &access,
            colors.text_dim,
            transform.pre_translate((content.x0, here_rect.y0 + f64::from(name_h) + m.s1).into()),
            None,
            &tokens.typography.style(TextRole::Body),
        );
    }

    /// The keyboard lands on the room you are in when the overview opens,
    /// so it carries the selection a list row does: without it the first
    /// arrow would seem to come from nowhere.
    fn paint_here_selection(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        content: Rect,
        here_rect: Rect,
        hi: usize,
    ) {
        if self.selection != Some(hi) {
            return;
        }
        let s2 = self.metrics.s2;
        // It stands `s2` off the block's text and stops short of the
        // block's bottom air, so it does not run into the caption beneath.
        let sel = Rect::new(
            content.x0 - s2,
            here_rect.y0 - s2,
            content.x1 + s2,
            here_rect.y1 - s2,
        );
        let fill = theme
            .tokens
            .colors
            .lift_on(ui::chrome::shell::panel_ground(theme), 1.0);
        ui::surface::card(scene, transform, sel, self.metrics.row_radius, fill);
        if self.window_selection.is_none() {
            let focus = if self.scrolls_whole_panel() {
                sel.intersect(content)
            } else {
                sel
            };
            ui::widget::paint_focus_ring(
                scene,
                transform,
                focus.inset(-2.0),
                (self.metrics.row_radius - 2.0).max(0.0),
                theme,
            );
        }
    }

    /// What leaves this room: the share list, or nothing when it shares
    /// nothing.
    fn paint_shares_section(&self, ctx: &mut ui::paint::PaintCtx<'_>, content: Rect) {
        let Some(card) = self.here_card().filter(|card| !card.shares.is_empty()) else {
            return;
        };
        let (scene, transform, text, theme) =
            (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
        let tokens = &theme.tokens;
        let m = self.metrics;
        let top = self.shares_top();
        let caption = tokens.typography.style(TextRole::Caption);
        text.draw_style(
            scene,
            &t("overview-shared-out"),
            tokens.colors.text_tertiary,
            transform.pre_translate((content.x0, top + m.caption_text_dy).into()),
            None,
            &caption,
        );
        let glyph = 14.0_f64;
        let mut y = top + m.caption_h;
        let shown = card.shares.len().min(SHARES_MAX_ROWS);
        for share in card.shares.iter().take(shown) {
            paint_share_row(
                &mut ui::paint::PaintCtx::new(scene, transform, text, theme),
                Rect::new(content.x0, y, content.x1, y + m.share_row_h),
                share,
                glyph,
            );
            y += m.share_row_h;
        }
        let hidden = card.shares.len().saturating_sub(shown);
        if hidden > 0 {
            let more = t_args("overview-more-shares", &[("count", hidden.into())]);
            let (_, more_h) = text.measure_style(&more, None, &caption);
            text.draw_style(
                scene,
                &more,
                tokens.colors.text_tertiary,
                transform.pre_translate(
                    (
                        content.x0 + glyph + m.s2,
                        y + (m.share_row_h - f64::from(more_h)) / 2.0,
                    )
                        .into(),
                ),
                None,
                &caption,
            );
        }
    }

    /// Where else you can go: one row per room, skipping those panned out
    /// of the band.
    fn paint_go_to_list(&self, ctx: &mut ui::paint::PaintCtx<'_>, band: Rect) {
        // A carried window shows the room it would land in as chosen.
        let drop_target = self.drop_target();
        for (idx, card) in self.desk.cards.iter().enumerate() {
            if self.slot_of(idx).is_none() {
                continue;
            }
            let rect = self.row_rect(idx);
            if rect.y1 < band.y0 || rect.y0 > band.y1 {
                continue;
            }
            let leave = self.leave_rect(idx);
            let trailing = leave.map_or(0.0, |leave| leave.width() + self.metrics.s3);
            self.paint_room_row(
                &mut ui::paint::PaintCtx::new(
                    &mut *ctx.scene,
                    ctx.transform,
                    &mut *ctx.text,
                    ctx.theme,
                ),
                rect,
                (idx, card),
                self.selection == Some(idx) || drop_target == Some(idx),
                trailing,
            );
            if let Some(leave) = leave {
                self.paint_leave(ctx, leave);
            }
        }
    }

    /// One room in the "go to" list; `trailing` is kept free at its end for
    /// a control of its own (a site workspace's Leave).
    fn paint_room_row(
        &self,
        ctx: &mut ui::paint::PaintCtx<'_>,
        rect: Rect,
        (idx, card): (usize, &RoomCard),
        selected: bool,
        trailing: f64,
    ) {
        let (scene, transform, text, theme) =
            (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let ground = ui::chrome::shell::panel_ground(theme);
        let hover = self.cursor.is_some_and(|c| rect.contains(c));
        let m = self.metrics;
        // A room row is a control you press: which one is chosen is the
        // shared selection tier, not an accent.
        ui::chrome::selection_tier(
            theme,
            ui::chrome::SelectionState::new(selected, hover, false),
        )
        .paint(scene, transform, rect, m.row_radius);
        if !selected && !hover {
            // The whole row is the target: a derived hairline keeps it
            // legible without giving it a body of its own.
            ui::chrome::shell::paint_hairline_outline(
                scene,
                transform,
                &RoundedRect::from_rect(rect, m.row_radius),
                ground,
            );
        }
        let x0 = rect.x0 + m.row_pad;
        let x1 = rect.x1 - m.row_pad - trailing;
        let faces_left = paint_face_stack(
            &mut ui::paint::PaintCtx::new(scene, transform, text, theme),
            Point::new(x1, rect.center().y),
            &card.people,
            self.colors(),
            ground,
        );
        let marks_left = paint_app_marks(
            scene,
            transform,
            theme,
            (faces_left - m.s3, rect.center().y),
            &card.apps(),
        );
        let text_w = (marks_left - m.s3 - x0).max(40.0);
        let name_style = tokens.typography.style(TextRole::Heading);
        let name = text.elide_to_width_style(&card.name, f32_of_f64(text_w), &name_style);
        let (_, name_h) = text.measure_style(&name, None, &name_style);
        let sub_style = tokens.typography.style(TextRole::Caption);
        // Who is in the room now outranks who could be; access is what the
        // line says when it is empty.
        let mut sub = self.desk.sites.get(&card.id).cloned().unwrap_or_else(|| {
            crate::overview::room_presence_label(card)
                .unwrap_or_else(|| crate::overview::room_access_label(card))
        });
        if let Some(shares) = crate::overview::room_shares_label(card) {
            sub = t_args(
                "overview-two-facts",
                &[("first", sub.into()), ("second", shares.into())],
            );
        }
        let sub = text.elide_to_width_style(&sub, f32_of_f64(text_w), &sub_style);
        let (_, sub_h) = text.measure_style(&sub, None, &sub_style);
        let stack_h = f64::from(name_h) + m.s1 + f64::from(sub_h);
        let top = rect.center().y - stack_h / 2.0;
        text.draw_style(
            scene,
            &name,
            colors.text_primary,
            transform.pre_translate((x0, top).into()),
            None,
            &name_style,
        );
        text.draw_style(
            scene,
            &sub,
            colors.text_dim,
            transform.pre_translate((x0, top + f64::from(name_h) + m.s1).into()),
            None,
            &sub_style,
        );
        if self.selection == Some(idx) && self.window_selection.is_none() {
            ui::widget::paint_focus_ring(
                scene,
                transform,
                rect.inset(-2.0),
                (m.row_radius - 2.0).max(0.0),
                theme,
            );
        }
    }

    /// "New workspace" or "Join a workspace…": a row of the list like any
    /// room, since making or joining one is somewhere else to go.
    fn paint_list_row(
        &self,
        ctx: &mut ui::paint::PaintCtx<'_>,
        rect: Option<Rect>,
        slot: usize,
        (label, icon): (&str, ui::icons::Icon),
    ) {
        use ui::widget::{Interaction, ListRowFace, RowLeading};
        let Some(rect) = rect else {
            return;
        };
        let focused = self.selection == Some(slot) && self.window_selection.is_none();
        ListRowFace::new(label)
            .leading(RowLeading::Icon(icon))
            .selected(focused)
            .interaction(Interaction {
                hovered: self.cursor.is_some_and(|cursor| rect.contains(cursor)),
                focused,
                ..Interaction::default()
            })
            .paint(ctx, rect);
    }

    // ---- The picture ----

    /// What the picture shows beside the windows' pictures at the zoom's
    /// `progress`: each strip's name and controls, an empty strip's slot,
    /// each window's title (flying with its picture), the selection at
    /// rest, and every picture the host does not draw itself. Returns the
    /// pictures for the host to draw.
    pub(super) fn paint_picture(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        progress: f64,
    ) -> Vec<WindowPicture> {
        let picture = self.picture();
        if picture.strips.is_empty() {
            return Vec::new();
        }
        let p = progress.clamp(0.0, 1.0);
        // In flight the windows cross the whole display; at rest they keep
        // to where the picture can be seen.
        let clip = if p < 1.0 { self.rect } else { self.grid_clip() };
        scene.push_clip_layer(Fill::NonZero, transform, &clip);
        // Names, titles and controls arrive once the windows mostly have.
        let chrome = if theme.reduce_motion {
            p
        } else {
            ui::motion::stage(p, 0.5, 1.0)
        };
        let mut pictures = Vec::new();
        let carried = self
            .press
            .filter(crate::overview::OverviewPress::is_dragging)
            .and_then(|press| press.tab);
        for si in 0..picture.strips.len() {
            self.paint_strip_label(
                &mut ui::paint::PaintCtx::new(scene, transform, text, theme),
                &picture,
                si,
                ui::motion::unit_opacity(chrome),
            );
            let (Some(strip), Some(place)) = (picture.strips.get(si), picture.places.get(si))
            else {
                continue;
            };
            if let Some(empty) = place.empty {
                draw_empty_strip(
                    &mut ui::paint::PaintCtx::new(scene, transform, text, theme),
                    empty,
                    ui::motion::unit_opacity(chrome),
                );
            }
            for (wi, window) in strip.windows.iter().enumerate() {
                let (Some(well), Some(title)) = (place.wells.get(wi), place.titles.get(wi)) else {
                    continue;
                };
                let tab = window.address.tab;
                let front = strip.front.get(wi).copied().unwrap_or(true);
                let window_tile = Tile {
                    window,
                    front,
                    well: *well,
                    title: *title,
                    faces: strip.faces.get(wi).map_or(&[][..], Vec::as_slice),
                };
                let picture = WindowPicture {
                    tab,
                    rect: *well,
                    radius: self.grid.radius,
                    alpha: if carried == Some(tab) {
                        CARRIED_ALPHA
                    } else {
                        1.0
                    },
                    flies: front,
                    lifted: false,
                };
                self.paint_tile(
                    &mut ui::paint::PaintCtx::new(scene, transform, text, theme),
                    &window_tile,
                    &picture,
                    (p, chrome),
                );
                pictures.push(picture);
            }
        }
        if let Some(lifted) = self.carried_picture(&picture) {
            if !self.desk.pictured.contains(&lifted.tab)
                && let Some((_, _, window)) = picture.find(lifted.tab)
            {
                ui::surface::shadow(
                    scene,
                    transform,
                    lifted.rect,
                    lifted.radius,
                    &theme.tokens.elevation.floating,
                );
                let _ = draw_preview_well(
                    scene,
                    transform,
                    theme,
                    PreviewWell {
                        rect: lifted.rect,
                        radius: lifted.radius,
                        icon: self.metrics.icon_lg,
                    },
                    window,
                    &mut NoThumbnails,
                );
            }
            pictures.push(lifted);
        }
        scene.pop_layer();
        pictures
    }

    /// Where the window being carried is drawn: centred under the pointer
    /// or finger, its picture's size.
    fn carried_picture(&self, picture: &Picture) -> Option<WindowPicture> {
        let press = self
            .press
            .filter(crate::overview::OverviewPress::is_dragging)?;
        let (si, wi, window) = picture.find(press.tab?)?;
        let well = picture.places.get(si)?.wells.get(wi)?;
        let (w, h) = (well.width(), well.height());
        Some(WindowPicture {
            tab: window.address.tab,
            rect: Rect::new(
                press.at.x - w / 2.0,
                press.at.y - h / 2.0,
                press.at.x + w / 2.0,
                press.at.y + h / 2.0,
            ),
            radius: self.grid.radius,
            alpha: 1.0,
            flies: false,
            lifted: true,
        })
    }

    /// Where `picture` is drawn at `progress`: flying from where its window
    /// stands when the host says, else resting where it ends.
    pub(super) fn flown(&self, picture: &WindowPicture, progress: f64) -> (Rect, f32) {
        let source = (picture.flies && !self.reduce_motion)
            .then(|| {
                self.flight
                    .as_ref()
                    .and_then(|flight| flight.source(picture.tab))
            })
            .flatten();
        match source {
            Some(from) => (
                ui::motion::Zoom::rect_at(from, picture.rect, progress),
                picture.alpha,
            ),
            None => (
                picture.rect,
                picture.alpha * ui::motion::unit_opacity(progress),
            ),
        }
    }

    /// One pictured window's selection at rest, its picture when the host
    /// draws none, and its title flying with it.
    fn paint_tile(
        &self,
        ctx: &mut ui::paint::PaintCtx<'_>,
        tile: &Tile<'_>,
        picture: &WindowPicture,
        (progress, chrome): (f64, f64),
    ) {
        let (rect, alpha) = self.flown(picture, progress);
        if alpha <= 0.0 {
            return;
        }
        let (scene, transform, theme) = (&mut *ctx.scene, ctx.transform, ctx.theme);
        let g = self.grid;
        let s1 = self.metrics.s1;
        let tab = tile.window.address.tab;
        let bounds = tile.well.union(tile.title);
        let layered = alpha < 1.0;
        if layered {
            scene.push_layer(
                Fill::NonZero,
                BlendMode::default(),
                alpha,
                transform,
                &rect.union(bounds).inflate(s1 * 2.0, s1 * 2.0),
            );
        }
        let carried = picture.alpha < 1.0;
        let state = ui::chrome::SelectionState::new(
            self.window_selection == Some(tab),
            self.cursor.is_some_and(|c| bounds.contains(c)),
            self.press.and_then(|press| press.tab) == Some(tab),
        );
        // Selection and focus belong to the window at rest.
        if progress >= 1.0 && !carried && (state.active || state.hover || state.pressed) {
            ui::chrome::selection_tier(theme, state).paint(
                scene,
                transform,
                bounds.inflate(s1, s1),
                g.radius + s1,
            );
            if state.active {
                ui::widget::paint_focus_ring(
                    scene,
                    transform,
                    bounds.inflate(s1, s1),
                    g.radius + s1,
                    theme,
                );
            }
        }
        if !self.desk.pictured.contains(&tab) {
            let radius = if rect == tile.well {
                g.radius
            } else {
                ui::motion::Zoom::radius_at(g.window_radius, g.radius, progress)
            };
            let _ = draw_preview_well(
                scene,
                transform,
                theme,
                PreviewWell {
                    rect,
                    radius,
                    icon: self.metrics.icon_lg.min(rect.height() * 0.6),
                },
                tile.window,
                &mut NoThumbnails,
            );
        }
        if chrome > 0.0 {
            draw_tile_title(
                &mut ui::paint::PaintCtx::new(scene, transform, &mut *ctx.text, theme),
                tile,
                rect,
                (ui::motion::unit_opacity(chrome), state.active),
            );
        }
        if layered {
            scene.pop_layer();
        }
    }

    /// Strip `si`'s name line: its name (or the field while it is typed)
    /// and its controls.
    fn paint_strip_label(
        &mut self,
        ctx: &mut ui::paint::PaintCtx<'_>,
        picture: &Picture,
        si: usize,
        alpha: f32,
    ) {
        let (Some(strip), Some(place)) = (picture.strips.get(si), picture.places.get(si)) else {
            return;
        };
        if alpha <= 0.0 {
            return;
        }
        let (scene, transform, theme) = (&mut *ctx.scene, ctx.transform, ctx.theme);
        scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            alpha,
            transform,
            &place.label,
        );
        let naming = self
            .naming
            .as_ref()
            .is_some_and(|naming| naming.strip == strip.id);
        if naming {
            if let Some(field) = self.naming_field(picture, si) {
                self.paint_naming_field(
                    &mut ui::paint::PaintCtx::new(scene, transform, &mut *ctx.text, theme),
                    field,
                );
            }
        } else {
            let controls = self.strip_controls(picture, si);
            let shown = self.strip_controls_shown(picture, si);
            let end = controls
                .first()
                .filter(|_| shown)
                .map_or(place.label.x1, |(_, rect)| rect.x0 - self.metrics.s2);
            draw_strip_name(
                &mut ui::paint::PaintCtx::new(scene, transform, &mut *ctx.text, theme),
                place.label,
                end,
                (&strip.name, strip.focused),
            );
            if shown {
                for (control, rect) in controls {
                    let name = control.name();
                    Button::paint(
                        scene,
                        transform,
                        &mut *ctx.text,
                        theme,
                        &IconButton::face(rect, control.icon(), &name)
                            .control_size(ControlSize::Small)
                            .hovered(self.cursor.is_some_and(|p| rect.contains(p))),
                    );
                }
            }
        }
        scene.pop_layer();
    }

    /// The name field over a strip's name line.
    fn paint_naming_field(&mut self, ctx: &mut ui::paint::PaintCtx<'_>, field: Rect) {
        let Some(naming) = self.naming.as_mut() else {
            return;
        };
        let (scene, transform, theme) = (&mut *ctx.scene, ctx.transform, ctx.theme);
        let placeholder = t("strip-switcher-name-placeholder");
        let face = FieldFace::new(field, &naming.text)
            .placeholder(&placeholder)
            .text_role(TextRole::Label)
            .focused(true);
        if naming.text.is_empty() {
            face.paint(scene, transform, &mut *ctx.text, theme);
        } else {
            face.paint_chrome(scene, transform, theme);
        }
        let input = naming.input.get_or_insert_with(|| {
            TextInput::new(Vec::new())
                .with_plain_chrome()
                .with_spellcheck(false)
                .with_autocorrect(false)
                .with_text_role(TextRole::Label)
        });
        input.set_text(&naming.text);
        input.set_selection(naming.selection.anchor, naming.selection.cursor);
        input.set_focus(true);
        input.apply_layout_external(face.text_rect(theme));
        let input_theme = theme
            .clone()
            .with_text_scale(theme.text_scale * theme.ui_scale);
        input.draw(
            scene,
            transform,
            &mut *ctx.text,
            &input_theme,
            &PresenceCtx::default(),
        );
    }
}

/// An overlapping stack of member faces, right-aligned at `anchor.x` and
/// centred on `anchor.y`, on `ground` (the separator ring is a hole punched
/// in the face below). An absent member stays at reduced strength rather
/// than disappearing. Returns the stack's left edge.
fn paint_face_stack(
    ctx: &mut ui::paint::PaintCtx<'_>,
    anchor: Point,
    people: &[RoomPerson],
    colors_for: &HashMap<String, (u8, u8, u8)>,
    ground: Color,
) -> f64 {
    let (right_x, cy) = (anchor.x, anchor.y);
    // Each face stands three quarters of a face past the one before.
    let face = f64::from(ctx.theme.tokens.metrics.avatar_inline);
    let r = face / 2.0;
    let step = face * 0.75;
    let shown = people.len().min(FACES_MAX);
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
        // monogram picks its ink from the fill it is drawn on.
        let fill = if person.present {
            fill
        } else {
            ui::tokens::colors::mix8(fill, ground, 1.0 - FACE_AWAY_ALPHA)
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
        ui::widget::AvatarFace::paint_overflow(
            ctx,
            Rect::new(right - r, cy - r, right + r, cy + r),
            overflow,
            Some(ground),
        );
    }
    right - f64_of_usize(slots.saturating_sub(1)) * step - r
}

/// Marks of the apps open in a room, ending at `right` and centred on
/// `center_y`, so a room is recognisable by what is in it. Returns the left
/// edge of what was drawn (`right` when the room is empty).
fn paint_app_marks(
    scene: &mut Scene,
    transform: Affine,
    theme: &Theme,
    (right, center_y): (f64, f64),
    apps: &[&WindowApp],
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

/// One "shared out of here" row: an icon, who holds it, and what.
fn paint_share_row(ctx: &mut ui::paint::PaintCtx<'_>, row: Rect, share: &RoomShare, glyph: f64) {
    let (scene, transform, text, theme) =
        (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
    let tokens = &theme.tokens;
    let colors = &tokens.colors;
    let body = tokens.typography.style(TextRole::Body);
    let (_, body_h) = text.measure_style("Ag", None, &body);
    let body_top = row.y0 + (row.height() - f64::from(body_h)) / 2.0;
    let cy = row.center().y;
    ui::icons::draw_icon(
        scene,
        transform,
        Rect::new(row.x0, cy - glyph / 2.0, row.x0 + glyph, cy + glyph / 2.0),
        ui::icons::Icon::ExternalLink,
        colors.text_tertiary,
        ui::icons::LUCIDE_DEFAULT_STROKE_PX,
    );
    // Who holds it sits right; what is shared elides against it.
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
    let (with_w, with_h) = text.measure_style(&with, None, &with_style);
    text.draw_style(
        scene,
        &with,
        colors.text_dim,
        transform.pre_translate(
            (
                row.x1 - f64::from(with_w),
                row.y0 + (row.height() - f64::from(with_h)) / 2.0,
            )
                .into(),
        ),
        None,
        &with_style,
    );
    let s2 = f64::from(tokens.spacing.s2);
    let what_x = row.x0 + glyph + s2;
    let what_avail = (row.x1 - f64::from(with_w) - f64::from(tokens.spacing.s3) - what_x).max(40.0);
    let what = text.elide_to_width_style(&share.what, f32_of_f64(what_avail), &body);
    text.draw_style(
        scene,
        &what,
        colors.text_primary,
        transform.pre_translate((what_x, body_top).into()),
        None,
        &body,
    );
}
