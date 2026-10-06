//! The history screen: a commit-graph list on the left with branch/merge
//! lanes drawn as bezier-connected dots, and a commit-detail panel on the
//! right showing the full message, author, parents, refs and the changed
//! files of the selected commit.

use vello::kurbo::{BezPath, Point, Rect, Stroke};
use vello::peniko::Color;

use ui::paint::PaintCtx;
use ui::text_render::{TextRenderer, TextStyle, TextWeight};
use ui::theme::Theme;

use crate::draw;
use crate::graph::GraphLayout;
use crate::model::{Commit, RefKind, RefLabel, RepoModel};

// Ref-decorated commits use three text baselines (refs, summary, metadata).
// 56px clipped the metadata into the following row, especially in the compact
// overlay where labels wrap sooner. Keep one deliberate line of breathing room.
// This is the 1× pitch; the painted pitch is [`Metrics::row_h`].
const ROW_H: f64 = 68.0;
const LANE_W: f64 = 18.0;
const DOT_R: f64 = 5.0;
const GRAPH_PAD: f64 = 16.0;
/// Text sizes painted in a commit row — measured with these exact values.
/// Whole-number sizes stay inline at their call sites (the F1 spacing ratchet
/// counts named whole-number `f32` consts); the two fractional ones are named.
const META_DIM_FONT: f32 = 11.5;

/// Row metrics measured from the renderer the list paints with.
///
/// Refreshed at the top of every draw and handed back to the owner with the
/// row rects, so the graph edges, dots, three text lines, separators and the
/// scroll clamp share one pitch at every text scale. The rhythm fields hold
/// the row's vertical grammar: at 1× that is 9 above the ref pills, 3 to the
/// summary, 2 to the meta line and 4 below (9 + 18 + 3 + 16.25 + 2 + 15 + 4 =
/// 67.25 ≤ 68), so the 1× pitch is the historical 68 and every line sits where
/// it did.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Metrics {
    /// Row height; the list pitch is [`Metrics::pitch`] (row plus gap).
    pub row_h: f64,
    /// Gap between neighbouring rows (`s1`): HIG §7 — nothing touches.
    row_gap: f64,
    /// Ref pill height (`draw::pill_height` at the 12 px ref label).
    pill_h: f64,
    /// Measured summary and meta line heights.
    summary_h: f64,
    meta_h: f64,
    /// Space above the ref pills, between pills and summary, between summary
    /// and meta line, and below the meta line.
    pad_top: f64,
    ref_gap: f64,
    meta_gap: f64,
    pad_bottom: f64,
    /// Where the summary starts when the commit carries no ref pills.
    no_refs_top: f64,
}

impl Default for Metrics {
    fn default() -> Self {
        Self {
            row_h: ROW_H,
            row_gap: 4.0,
            pill_h: 18.0,
            summary_h: 16.25,
            meta_h: 15.0,
            pad_top: 9.0,
            ref_gap: 3.0,
            meta_gap: 2.0,
            pad_bottom: 4.0,
            no_refs_top: 10.0,
        }
    }
}

impl Metrics {
    /// Vertical distance between the tops of two neighbouring rows: the row
    /// plus the `s1` gap that keeps rows from touching.
    #[must_use]
    pub fn pitch(&self) -> f64 {
        self.row_h + self.row_gap
    }

    pub fn measure(tr: &mut TextRenderer, theme: &Theme) -> Self {
        let rhythm = Self::default();
        let pill_h = draw::pill_height(tr, 12.0);
        let summary_h = f64::from(tr.measure_weighted("Ag", 13.0, None, TextWeight::Medium).1);
        let meta_h = f64::from(tr.measure("Ag", 12.0, None).1);
        // The one on-grid gap reads its token; the rest are the row's own
        // off-grid rhythm, kept in `Default` where 1× reads them verbatim.
        let pad_bottom = f64::from(theme.tokens.spacing.s1);
        let row_h = (rhythm.pad_top
            + pill_h
            + rhythm.ref_gap
            + summary_h
            + rhythm.meta_gap
            + meta_h
            + pad_bottom)
            .max(ROW_H);
        Self {
            row_h,
            row_gap: f64::from(theme.tokens.spacing.s1),
            pill_h,
            summary_h,
            meta_h,
            pad_bottom,
            ..rhythm
        }
    }

    /// Top of the summary line: under the ref pills when any were painted.
    fn summary_top(&self, y: f64, has_refs: bool) -> f64 {
        if has_refs {
            y + self.pad_top + self.pill_h + self.ref_gap
        } else {
            y + self.no_refs_top
        }
    }
}

/// What one [`HistoryView::draw_list`] pass produced: the row rects for
/// hit-testing and the metrics the owner clamps scrolling with.
pub struct ListDraw {
    pub rows: Vec<Rect>,
    pub metrics: Metrics,
}

pub struct HistoryView;

impl HistoryView {
    /// Total list height at the row pitch of the last draw.
    pub fn content_height(model: &RepoModel, metrics: &Metrics) -> f64 {
        model.commits.len() as f64 * metrics.pitch() + 8.0
    }

    /// Draw the commit list with graph. Returns the row rects so the app
    /// can hit-test selection. `selected` is the index of the highlighted
    /// commit.
    pub fn draw_list(
        ctx: &mut PaintCtx<'_>,
        area: Rect,
        model: &RepoModel,
        layout: &GraphLayout,
        selected: usize,
        scroll: f64,
    ) -> ListDraw {
        let c = &ctx.theme.tokens.colors;
        ctx.fill_rect(area, c.surfaces.base);
        let m = Metrics::measure(ctx.text, ctx.theme);

        ctx.scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            ctx.transform,
            &area,
        );

        let graph_total_w = GRAPH_PAD + (layout.lanes.max(1) as f64) * LANE_W + 8.0;
        let pass = ListPass {
            area,
            layout,
            selected,
            text_x: area.x0 + graph_total_w + 6.0,
            m,
        };
        pass.draw_graph_edges(ctx, scroll);

        let mut rows: Vec<Rect> = Vec::new();
        for (row, commit) in model.commits.iter().enumerate() {
            let y = area.y0 - scroll + row as f64 * m.pitch();
            let rrect = Rect::new(area.x0, y, area.x1, y + m.row_h);
            rows.push(rrect);
            if y + m.row_h < area.y0 || y > area.y1 {
                continue;
            }
            pass.draw_commit_row(ctx, row, commit, rrect);
        }

        ctx.scene.pop_layer();
        ListDraw { rows, metrics: m }
    }

    /// Draw the detail panel for `commit` on the right.
    pub fn draw_detail(ctx: &mut PaintCtx<'_>, area: Rect, commit: &Commit, scroll: f64) {
        let c = &ctx.theme.tokens.colors;
        ctx.fill_rect(area, c.surfaces.raised);
        draw::vline(
            ctx,
            area.x0,
            area.y0,
            area.y1,
            draw::edge(c.surfaces.raised),
        );

        ctx.scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            ctx.transform,
            &area,
        );

        let pad = 20.0;
        let x = area.x0 + pad;
        let width = (area.width() - pad * 2.0) as f32;
        let at = |y: f64| Point::new(x, y);

        let mut y = area.y0 + pad - scroll;
        y = Self::draw_detail_summary(ctx, commit, at(y), width);
        y = Self::draw_detail_author(ctx, commit, at(y));
        y = Self::draw_detail_meta_grid(ctx, commit, at(y));

        y += 8.0;
        draw::hline(ctx, y, x, area.x1 - pad, draw::edge(c.surfaces.raised));
        y += 14.0;

        y = Self::draw_detail_body(ctx, commit, at(y), width);
        Self::draw_detail_files(ctx, commit, at(y), width, area.x1 - pad);

        ctx.scene.pop_layer();
    }

    /// The commit summary line at `origin`, wrapped at `width` (part of
    /// [`Self::draw_detail`]). Returns the `y` for the next section.
    fn draw_detail_summary(
        ctx: &mut PaintCtx<'_>,
        commit: &Commit,
        origin: Point,
        width: f32,
    ) -> f64 {
        let c = &ctx.theme.tokens.colors;
        let style = TextStyle::new(16.0).with_weight(TextWeight::Semibold);
        ctx.text(&commit.summary, &style, c.text_primary, origin, Some(width));
        let (_, summary_h) = ctx.text.measure_style(&commit.summary, Some(width), &style);
        origin.y + f64::from(summary_h) + 14.0
    }

    /// The author block with avatar at `origin` (part of
    /// [`Self::draw_detail`]). Returns the `y` for the next section.
    fn draw_detail_author(ctx: &mut PaintCtx<'_>, commit: &Commit, origin: Point) -> f64 {
        let theme = ctx.theme;
        let c = &theme.tokens.colors;
        let acolor = draw::author_color(theme, &commit.author_email);
        draw::avatar(ctx, origin, 32.0, &commit.author_name, acolor);
        let text_x = origin.x + 42.0;
        ctx.text(
            &commit.author_name,
            &TextStyle::new(13.0).with_weight(TextWeight::Medium),
            c.text_primary,
            Point::new(text_x, origin.y),
            None,
        );
        ctx.text(
            &commit.author_email,
            &TextStyle::new(12.0),
            c.text_dim,
            Point::new(text_x, origin.y + 17.0),
            None,
        );
        origin.y + 44.0
    }

    /// The commit / date / parents / refs metadata grid at `origin` (part of
    /// [`Self::draw_detail`]). Returns the `y` for the next section.
    fn draw_detail_meta_grid(ctx: &mut PaintCtx<'_>, commit: &Commit, origin: Point) -> f64 {
        let x = origin.x;
        let short: String = commit.id.chars().take(12).collect();
        let mut y = meta_row(ctx, origin, "commit", &short, true);
        y = meta_row(ctx, Point::new(x, y), "date", &commit.when, false);
        if !commit.parents.is_empty() {
            let parents: Vec<String> = commit
                .parents
                .iter()
                .map(|p| p.chars().take(7).collect())
                .collect();
            y = meta_row(ctx, Point::new(x, y), "parents", &parents.join(", "), true);
        }
        if !commit.refs.is_empty() {
            let refs: Vec<String> = commit.refs.iter().map(|r| r.name.clone()).collect();
            y = meta_row(ctx, Point::new(x, y), "refs", &refs.join(", "), false);
        }
        y
    }

    /// The full commit body message at `origin`, wrapped at `width`, if any
    /// (part of [`Self::draw_detail`]). Returns the `y` for the next section
    /// (unchanged when there is no body).
    fn draw_detail_body(ctx: &mut PaintCtx<'_>, commit: &Commit, origin: Point, width: f32) -> f64 {
        let c = &ctx.theme.tokens.colors;
        if commit.body.is_empty() {
            return origin.y;
        }
        let style = TextStyle::new(13.0);
        ctx.text(&commit.body, &style, c.neutral[7], origin, Some(width));
        let (_, body_h) = ctx.text.measure_style(&commit.body, Some(width), &style);
        origin.y + f64::from(body_h) + 18.0
    }

    /// The changed-files list at `origin`, if any (part of
    /// [`Self::draw_detail`]). Paths wrap inside `width`; the +/- counts are
    /// right-aligned to `right_x`.
    fn draw_detail_files(
        ctx: &mut PaintCtx<'_>,
        commit: &Commit,
        origin: Point,
        width: f32,
        right_x: f64,
    ) {
        let theme = ctx.theme;
        let c = &theme.tokens.colors;
        if commit.files.is_empty() {
            return;
        }
        let x = origin.x;
        let mut y = origin.y;
        ctx.text(
            "Changed files",
            &TextStyle::new(12.0).with_weight(TextWeight::Semibold),
            c.text_dim,
            origin,
            None,
        );
        y += 24.0;
        for file in &commit.files {
            let badge = file.kind.badge();
            let badge_color = draw::kind_color(theme, file.kind);
            draw::mono(ctx, badge, 12.0, badge_color, Point::new(x, y));
            ctx.text(
                &file.path,
                &TextStyle::new(12.0),
                c.text_primary,
                Point::new(x + 18.0, y),
                Some(width - 80.0),
            );
            let counts = format!("+{} -{}", file.additions, file.deletions);
            let counts_w = f64::from(draw::mono_width(ctx.text, &counts, 11.5));
            draw::mono(
                ctx,
                &counts,
                11.5,
                c.text_dim,
                Point::new(right_x - counts_w, y + 1.0),
            );
            y += 24.0;
        }
    }
}

/// One paint pass over the commit list: its viewport, the graph it draws
/// lanes from, the highlighted row, where row text starts, and the measured
/// row metrics.
struct ListPass<'a> {
    area: Rect,
    layout: &'a GraphLayout,
    selected: usize,
    text_x: f64,
    m: Metrics,
}

impl ListPass<'_> {
    /// The branch/merge lane edges, drawn before the dots so dots paint on
    /// top (part of [`HistoryView::draw_list`]).
    fn draw_graph_edges(&self, ctx: &mut PaintCtx<'_>, scroll: f64) {
        let area = self.area;
        let row_h = self.m.row_h;
        let pitch = self.m.pitch();
        let graph_x = area.x0 + GRAPH_PAD;
        let lane_x = |lane: usize| graph_x + lane as f64 * LANE_W + LANE_W / 2.0;
        let row_center_y = |row: usize| area.y0 - scroll + row as f64 * pitch + row_h / 2.0;
        for edge in &self.layout.edges {
            let y0 = row_center_y(edge.from_row);
            let y1 = row_center_y(edge.to_row);
            // Cull if both endpoints are far off-screen.
            if y0.max(y1) < area.y0 - row_h || y0.min(y1) > area.y1 + row_h {
                continue;
            }
            let x0 = lane_x(edge.from_lane);
            let x1 = lane_x(edge.to_lane);
            let color = draw::lane_color(ctx.theme, edge.to_lane.max(edge.from_lane));
            let mut path = BezPath::new();
            path.move_to(Point::new(x0, y0));
            if (x0 - x1).abs() < 0.5 {
                path.line_to(Point::new(x1, y1));
            } else {
                // S-curve between lanes.
                let my = f64::midpoint(y0, y1);
                path.curve_to(Point::new(x0, my), Point::new(x1, my), Point::new(x1, y1));
            }
            ctx.scene
                .stroke(&Stroke::new(2.0), ctx.transform, color, None, &path);
        }
    }

    /// One commit row in `rrect`: selection tint, graph dot, ref pills,
    /// summary, author/time/hash meta line, and the row separator (part of
    /// [`HistoryView::draw_list`]). Split into [`Self::draw_commit_dot`] and
    /// [`Self::draw_commit_row_text`] to keep each half under the
    /// line-count lint.
    fn draw_commit_row(&self, ctx: &mut PaintCtx<'_>, row: usize, commit: &Commit, rrect: Rect) {
        let theme = ctx.theme;
        if row == self.selected {
            // Keep the selected commit legible and calm. The accent rail is
            // enough to identify selection while preserving graph lane
            // colours for topology rather than state.
            ctx.fill_rect(rrect, theme.tokens.colors.lift(0.72));
            ctx.fill_rect(
                Rect::new(rrect.x0, rrect.y0, rrect.x0 + 3.0, rrect.y1),
                theme.accent_primary,
            );
        }
        self.draw_commit_dot(ctx, row, commit, rrect.y0);
        self.draw_commit_row_text(ctx, commit, rrect.y0);
    }

    /// The graph dot for the commit row at `y` — filled lane-coloured
    /// circle, hollow centre for a merge (part of [`Self::draw_commit_row`]).
    fn draw_commit_dot(&self, ctx: &mut PaintCtx<'_>, row: usize, commit: &Commit, y: f64) {
        let theme = ctx.theme;
        let graph_x = self.area.x0 + GRAPH_PAD;
        let lane = self.layout.lane_of.get(row).copied().unwrap_or(0);
        let cx = graph_x + lane as f64 * LANE_W + LANE_W / 2.0;
        let cy = y + self.m.row_h / 2.0;
        let dot_color = draw::lane_color(theme, lane);
        let is_merge = commit.parents.len() > 1;
        let dr = if is_merge { DOT_R + 1.5 } else { DOT_R };
        ctx.fill_round(Rect::new(cx - dr, cy - dr, cx + dr, cy + dr), dr, dot_color);
        if is_merge {
            // Hollow center to mark a merge.
            ctx.fill_round(
                Rect::new(cx - 2.0, cy - 2.0, cx + 2.0, cy + 2.0),
                2.0,
                theme.tokens.colors.surfaces.base,
            );
        }
    }

    /// Ref pills, summary, author/time/hash meta line, and the row
    /// separator for the commit row at `y` (part of [`Self::draw_commit_row`]).
    fn draw_commit_row_text(&self, ctx: &mut PaintCtx<'_>, commit: &Commit, y: f64) {
        let area = self.area;
        let m = &self.m;
        let theme = ctx.theme;
        let c = &theme.tokens.colors;
        let gap = f64::from(theme.tokens.spacing.s2);
        // Short hash, right-aligned on the first text line; the ref pills and
        // summary stop before it.
        let short: String = commit.id.chars().take(7).collect();
        let hw = f64::from(draw::mono_width(ctx.text, &short, 12.0));
        let hash_x = area.x1 - hw - 14.0;

        let painted_any = draw_ref_pills(
            ctx,
            &commit.refs,
            Point::new(self.text_x, y + m.pad_top),
            hash_x - gap,
        );

        // Summary. If ref pills were painted, draw it on the second line;
        // else the first.
        let sx = self.text_x;
        let sy = m.summary_top(y, painted_any);
        let avail = if painted_any {
            area.x1 - 14.0 - sx
        } else {
            hash_x - gap - sx
        };
        let summary = ctx.text.elide_to_width_weighted(
            &commit.summary,
            13.0,
            avail.max(12.0) as f32,
            TextWeight::Medium,
        );
        ctx.text(
            &summary,
            &TextStyle::new(13.0).with_weight(TextWeight::Medium),
            c.text_primary,
            Point::new(sx, sy),
            None,
        );

        // Author + time + short hash on the meta line. Author identity is
        // carried by the avatar; keeping the name neutral avoids implying
        // that the author's hue is a status/severity colour like graph lanes
        // and file badges.
        let meta_y = sy + m.summary_h + m.meta_gap;
        ctx.text(
            &commit.author_name,
            &TextStyle::new(12.0),
            c.text_primary,
            Point::new(sx, meta_y),
            None,
        );
        let aw = f64::from(ctx.text.measure(&commit.author_name, 12.0, None).0);
        let meta = format!("·  {}", commit.when);
        let meta_x = sx + aw + gap;
        let meta = ctx.text.elide_to_width(
            &meta,
            META_DIM_FONT,
            (area.x1 - 14.0 - meta_x).max(12.0) as f32,
        );
        // The dimmer, smaller time shares the author's centre line.
        let (_, mh) = ctx.text.measure(&meta, META_DIM_FONT, None);
        ctx.text(
            &meta,
            &TextStyle::new(META_DIM_FONT),
            c.text_dim,
            Point::new(meta_x, meta_y + (m.meta_h - f64::from(mh)) / 2.0),
            None,
        );

        draw::mono(
            ctx,
            &short,
            12.0,
            c.text_dim,
            Point::new(hash_x, y + m.no_refs_top),
        );

        // The row rule sits in the middle of the inter-row gap.
        draw::hline(
            ctx,
            y + m.row_h + m.row_gap / 2.0,
            area.x0 + 8.0,
            area.x1 - 8.0,
            c.lift(0.35),
        );
    }
}

/// Ref pills from `origin`, painted while they fit left of `limit_x`.
/// The first pill always paints — elided to the room left — so a commit
/// with a long branch name still shows that it carries a ref instead of
/// an empty first line; later pills that do not fit are dropped. Returns
/// whether any pill was painted.
fn draw_ref_pills(ctx: &mut PaintCtx<'_>, refs: &[RefLabel], origin: Point, limit_x: f64) -> bool {
    let mut px = origin.x;
    let mut painted_any = false;
    for r in refs {
        let (fg, bg) = ref_colors(ctx.theme, r.kind);
        let room = limit_x - px;
        let name = if draw::pill_width(ctx.text, &r.name, 12.0) <= room {
            r.name.clone()
        } else if painted_any {
            break;
        } else {
            let label_room = (room - draw::pill_width(ctx.text, "", 12.0)).max(0.0) as f32;
            ctx.text
                .elide_to_width_weighted(&r.name, 12.0, label_room, TextWeight::Medium)
        };
        let style = draw::PillStyle {
            size: 12.0,
            fg,
            bg,
            edge: draw::PillEdge::Bare,
        };
        px += draw::pill(ctx, Point::new(px, origin.y), &name, &style);
        painted_any = true;
    }
    painted_any
}

/// One `label  value` line of the detail metadata grid at `origin`; `mono`
/// sets the value in the code face. Returns the `y` of the next line.
fn meta_row(ctx: &mut PaintCtx<'_>, origin: Point, label: &str, value: &str, mono: bool) -> f64 {
    let c = &ctx.theme.tokens.colors;
    ctx.text(label, &TextStyle::new(12.0), c.text_dim, origin, None);
    let value_origin = Point::new(origin.x + 70.0, origin.y);
    if mono {
        draw::mono(ctx, value, 12.0, c.text_primary, value_origin);
    } else {
        ctx.text(
            value,
            &TextStyle::new(12.0),
            c.text_primary,
            value_origin,
            None,
        );
    }
    origin.y + 22.0
}

fn ref_colors(theme: &Theme, kind: RefKind) -> (Color, Color) {
    let c = &theme.tokens.colors;
    match kind {
        RefKind::Head => (
            c.on_accent(ui::tokens::colors::ON_FILL_MIN_CONTRAST),
            theme.accent_primary,
        ),
        RefKind::LocalBranch => (c.text_primary, c.accent[2]),
        RefKind::RemoteBranch => {
            let g = c.semantic.success;
            // The pill is a 60/255 wash, so ink is measured on the composite
            // over both the list ground and the selected-row lift.
            let wash = 60.0_f32 / 255.0;
            let on_row = ui::tokens::colors::over(c.surfaces.base, g, wash);
            let on_selected = ui::tokens::colors::over(c.lift(0.72), g, wash);
            (
                c.ink_on_any(
                    &[on_row, on_selected],
                    c.text_primary,
                    ui::tokens::colors::ON_FILL_MIN_CONTRAST,
                ),
                ui::paint::with_alpha(g, 60),
            )
        }
        RefKind::Tag => {
            let a = c.semantic.warning;
            (c.surfaces.base, ui::paint::with_alpha(a, 220))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// History rows: the measured row reproduces 68 px at 1x and grows at 2x, and
    /// rows keep an `s1` gap on every preset. Each phase is a former test with its own fresh fixture.
    #[test]
    fn history_rows_follow_text_scale_and_keep_token_gaps() {
        row_metrics_match_the_constant_at_1x_and_follow_the_text_scale();
        commit_rows_keep_a_token_gap_on_every_preset();
    }

    /// The measured row reproduces the historical 68 px pitch at 1× and grows
    /// with its three text lines at 2×, so ref pills, summary and meta line
    /// never share pixels.
    #[expect(
        clippy::float_cmp,
        reason = "layout reproduces authored whole-pixel sizes and their sums exactly in binary floating point; exact equality is what the test checks"
    )]
    fn row_metrics_match_the_constant_at_1x_and_follow_the_text_scale() {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let one = Metrics::measure(&mut tr, &theme);
        assert_eq!(
            one,
            Metrics::default(),
            "1× measures exactly the historical rhythm"
        );
        assert_eq!(one.row_h, ROW_H);
        assert_eq!(
            one.pill_h, 18.0,
            "12 px ref label → the historical 18 px pill"
        );
        // With refs the summary starts where it did (y + 30); without refs it
        // stays on the first line.
        assert!((one.summary_top(0.0, true) - 30.0).abs() < 0.01);
        assert_eq!(one.summary_top(0.0, false), 10.0);

        tr.set_text_scale(2.0);
        let two = Metrics::measure(&mut tr, &theme);
        assert!(two.row_h > one.row_h && two.pill_h > one.pill_h);
        let meta_bottom = two.summary_top(0.0, true) + two.summary_h + two.meta_gap + two.meta_h;
        assert!(
            meta_bottom + two.pad_bottom <= two.row_h + 0.01,
            "the meta line ({meta_bottom}) must end inside the {} px row",
            two.row_h
        );
    }

    /// HIG §7 "nothing touches": the list pitch is the row plus an `s1`
    /// gap, so the graph dot centre, the row rect and the separator all
    /// agree and neighbouring commit rows never share pixels.
    fn commit_rows_keep_a_token_gap_on_every_preset() {
        for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
            let mut tr = TextRenderer::new();
            let m = Metrics::measure(&mut tr, &theme);
            let s1 = f64::from(theme.tokens.spacing.s1);
            assert!(
                m.row_gap >= s1 && s1 >= 4.0,
                "{}: row gap {}",
                theme.name,
                m.row_gap
            );
            assert!((m.pitch() - (m.row_h + m.row_gap)).abs() < 0.01);
            let model = crate::sample::model().0;
            let expected = model.commits.len() as f64 * m.pitch() + 8.0;
            assert!((HistoryView::content_height(&model, &m) - expected).abs() < 0.01);
        }
    }
}
