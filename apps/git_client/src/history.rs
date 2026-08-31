//! The history screen: a commit-graph list on the left with branch/merge
//! lanes drawn as bezier-connected dots, and a commit-detail panel on the
//! right showing the full message, author, parents, refs and the changed
//! files of the selected commit.

use vello::Scene;
use vello::kurbo::{Affine, BezPath, Point, Rect, Stroke};
use vello::peniko::Color;

use ui::ColorCompat;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;

use crate::draw;
use crate::graph::GraphLayout;
use crate::model::{Commit, RefKind, RepoModel};

const ROW_H: f64 = 56.0;
const LANE_W: f64 = 18.0;
const DOT_R: f64 = 5.0;
const GRAPH_PAD: f64 = 16.0;

pub struct HistoryView;

impl HistoryView {
    pub fn content_height(model: &RepoModel) -> f64 {
        model.commits.len() as f64 * ROW_H + 8.0
    }

    /// Draw the commit list with graph. Returns the row rects so the app
    /// can hit-test selection. `selected` is the index of the highlighted
    /// commit.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_list(
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        area: Rect,
        model: &RepoModel,
        layout: &GraphLayout,
        selected: usize,
        scroll: f64,
    ) -> Vec<Rect> {
        let c = &theme.tokens.colors;
        draw::fill_rect(scene, t, area, c.surfaces.base);

        scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            t,
            &area,
        );

        Self::draw_graph_edges(scene, t, theme, area, layout, scroll);

        let graph_total_w = GRAPH_PAD + (layout.lanes.max(1) as f64) * LANE_W + 8.0;
        let text_x = area.x0 + graph_total_w + 6.0;
        let mut row_rects: Vec<Rect> = Vec::new();

        for (row, commit) in model.commits.iter().enumerate() {
            let y = area.y0 - scroll + row as f64 * ROW_H;
            let rrect = Rect::new(area.x0, y, area.x1, y + ROW_H);
            row_rects.push(rrect);
            if y + ROW_H < area.y0 || y > area.y1 {
                continue;
            }
            Self::draw_commit_row(
                scene, tr, t, theme, area, layout, commit, row, y, rrect, selected, text_x,
            );
        }

        scene.pop_layer();
        row_rects
    }

    /// The branch/merge lane edges, drawn before the dots so dots paint on
    /// top (part of [`Self::draw_list`]).
    fn draw_graph_edges(
        scene: &mut Scene,
        t: Affine,
        theme: &Theme,
        area: Rect,
        layout: &GraphLayout,
        scroll: f64,
    ) {
        let graph_x = area.x0 + GRAPH_PAD;
        let lane_x = |lane: usize| graph_x + lane as f64 * LANE_W + LANE_W / 2.0;
        let row_center_y = |row: usize| area.y0 - scroll + row as f64 * ROW_H + ROW_H / 2.0;
        for edge in &layout.edges {
            let y0 = row_center_y(edge.from_row);
            let y1 = row_center_y(edge.to_row);
            // Cull if both endpoints are far off-screen.
            if y0.max(y1) < area.y0 - ROW_H || y0.min(y1) > area.y1 + ROW_H {
                continue;
            }
            let x0 = lane_x(edge.from_lane);
            let x1 = lane_x(edge.to_lane);
            let color = draw::lane_color(theme, edge.to_lane.max(edge.from_lane));
            let mut path = BezPath::new();
            path.move_to(Point::new(x0, y0));
            if (x0 - x1).abs() < 0.5 {
                path.line_to(Point::new(x1, y1));
            } else {
                // S-curve between lanes.
                let my = f64::midpoint(y0, y1);
                path.curve_to(Point::new(x0, my), Point::new(x1, my), Point::new(x1, y1));
            }
            scene.stroke(&Stroke::new(2.0), t, color, None, &path);
        }
    }

    /// One commit row: selection tint, graph dot, ref pills, summary,
    /// author/time/hash meta line, and the row separator (part of
    /// [`Self::draw_list`]).
    #[allow(clippy::too_many_arguments)]
    fn draw_commit_row(
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        area: Rect,
        layout: &GraphLayout,
        commit: &Commit,
        row: usize,
        y: f64,
        rrect: Rect,
        selected: usize,
        text_x: f64,
    ) {
        let c = &theme.tokens.colors;
        if row == selected {
            draw::fill_rect(scene, t, rrect, c.accent[2]);
        }

        // Dot.
        let graph_x = area.x0 + GRAPH_PAD;
        let lane = layout.lane_of.get(row).copied().unwrap_or(0);
        let cx = graph_x + lane as f64 * LANE_W + LANE_W / 2.0;
        let cy = y + ROW_H / 2.0;
        let dot_color = draw::lane_color(theme, lane);
        let is_merge = commit.parents.len() > 1;
        let dr = if is_merge { DOT_R + 1.5 } else { DOT_R };
        draw::fill_round(
            scene,
            t,
            Rect::new(cx - dr, cy - dr, cx + dr, cy + dr),
            dr,
            dot_color,
        );
        if is_merge {
            // Hollow center to mark a merge.
            draw::fill_round(
                scene,
                t,
                Rect::new(cx - 2.0, cy - 2.0, cx + 2.0, cy + 2.0),
                2.0,
                c.surfaces.base,
            );
        }

        // Ref pills.
        let mut px = text_x;
        let pill_y = y + 9.0;
        for r in &commit.refs {
            let (fg, bg) = ref_colors(theme, r.kind);
            px += draw::pill(scene, tr, t, px, pill_y, 18.0, &r.name, fg, bg, None);
            if px > area.x1 - 200.0 {
                break;
            }
        }

        // Summary. If there are refs, draw it on the second line; else
        // the first.
        let sx = text_x;
        let sy = if commit.refs.is_empty() {
            y + 10.0
        } else {
            y + 30.0
        };
        let avail = area.x1 - sx - 90.0;
        draw::text_weighted(
            scene,
            tr,
            t,
            &commit.summary,
            13.0,
            c.text_primary,
            sx,
            sy,
            Some(avail as f32),
            TextWeight::Medium,
        );

        // Author + time + short hash on the meta line.
        let meta_y = sy + 18.0;
        let acolor = draw::author_color(theme, &commit.author_email);
        draw::text(
            scene,
            tr,
            t,
            &commit.author_name,
            11.0,
            acolor,
            sx,
            meta_y,
            None,
        );
        let aw = f64::from(tr.measure(&commit.author_name, 11.0, None).0);
        let meta = format!("·  {}", commit.when);
        draw::text(
            scene,
            tr,
            t,
            &meta,
            11.0,
            c.text_dim,
            sx + aw + 8.0,
            meta_y,
            None,
        );

        // Short hash, right-aligned.
        let short: String = commit.id.chars().take(7).collect();
        let hw = f64::from(draw::mono_width(tr, &short, 11.5));
        draw::mono(
            scene,
            tr,
            t,
            &short,
            11.5,
            c.neutral[5],
            area.x1 - hw - 14.0,
            y + 10.0,
        );

        draw::hline(
            scene,
            t,
            y + ROW_H,
            area.x0 + 8.0,
            area.x1 - 8.0,
            c.surface_white_5,
        );
    }

    /// Draw the detail panel for `commit` on the right.
    pub fn draw_detail(
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        area: Rect,
        commit: &Commit,
        scroll: f64,
    ) {
        let c = &theme.tokens.colors;
        draw::fill_rect(scene, t, area, c.surfaces.raised);
        draw::vline(scene, t, area.x0, area.y0, area.y1, c.border_standard);

        scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            t,
            &area,
        );

        let pad = 20.0;
        let mut y = area.y0 + pad - scroll;
        let x = area.x0 + pad;
        let w = (area.width() - pad * 2.0) as f32;

        y = Self::draw_detail_summary(scene, tr, t, theme, commit, x, y, w);
        y = Self::draw_detail_author(scene, tr, t, theme, commit, x, y);
        y = Self::draw_detail_meta_grid(scene, tr, t, theme, commit, x, y, w);

        y += 8.0;
        draw::hline(scene, t, y, x, area.x1 - pad, c.border_standard);
        y += 14.0;

        y = Self::draw_detail_body(scene, tr, t, theme, commit, x, y, w);
        Self::draw_detail_files(scene, tr, t, theme, area, commit, x, y, w, pad);

        scene.pop_layer();
    }

    /// The commit summary line (part of [`Self::draw_detail`]). Returns the
    /// `y` for the next section.
    fn draw_detail_summary(
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        commit: &Commit,
        x: f64,
        y: f64,
        w: f32,
    ) -> f64 {
        let c = &theme.tokens.colors;
        draw::text_weighted(
            scene,
            tr,
            t,
            &commit.summary,
            16.0,
            c.text_primary,
            x,
            y,
            Some(w),
            TextWeight::Semibold,
        );
        let sh = f64::from(
            tr.measure_weighted(&commit.summary, 16.0, Some(w), TextWeight::Semibold)
                .1,
        );
        y + sh + 14.0
    }

    /// The author block with avatar (part of [`Self::draw_detail`]).
    /// Returns the `y` for the next section.
    fn draw_detail_author(
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        commit: &Commit,
        x: f64,
        y: f64,
    ) -> f64 {
        let c = &theme.tokens.colors;
        let acolor = draw::author_color(theme, &commit.author_email);
        draw::avatar(scene, tr, t, theme, x, y, 32.0, &commit.author_name, acolor);
        draw::text_weighted(
            scene,
            tr,
            t,
            &commit.author_name,
            13.0,
            c.text_primary,
            x + 42.0,
            y,
            None,
            TextWeight::Medium,
        );
        draw::text(
            scene,
            tr,
            t,
            &commit.author_email,
            11.5,
            c.text_dim,
            x + 42.0,
            y + 17.0,
            None,
        );
        y + 44.0
    }

    /// The commit / date / parents / refs metadata grid (part of
    /// [`Self::draw_detail`]). Returns the `y` for the next section.
    fn draw_detail_meta_grid(
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        commit: &Commit,
        x: f64,
        y: f64,
        w: f32,
    ) -> f64 {
        let short: String = commit.id.chars().take(12).collect();
        let mut y = meta_row(scene, tr, t, theme, x, y, w, "commit", &short, true);
        y = meta_row(scene, tr, t, theme, x, y, w, "date", &commit.when, false);
        if !commit.parents.is_empty() {
            let parents: Vec<String> = commit
                .parents
                .iter()
                .map(|p| p.chars().take(7).collect())
                .collect();
            y = meta_row(
                scene,
                tr,
                t,
                theme,
                x,
                y,
                w,
                "parents",
                &parents.join(", "),
                true,
            );
        }
        if !commit.refs.is_empty() {
            let refs: Vec<String> = commit.refs.iter().map(|r| r.name.clone()).collect();
            y = meta_row(
                scene,
                tr,
                t,
                theme,
                x,
                y,
                w,
                "refs",
                &refs.join(", "),
                false,
            );
        }
        y
    }

    /// The full commit body message, if any (part of [`Self::draw_detail`]).
    /// Returns the `y` for the next section (unchanged when there is no
    /// body).
    fn draw_detail_body(
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        commit: &Commit,
        x: f64,
        y: f64,
        w: f32,
    ) -> f64 {
        let c = &theme.tokens.colors;
        if commit.body.is_empty() {
            return y;
        }
        draw::text(
            scene,
            tr,
            t,
            &commit.body,
            13.0,
            c.neutral[7],
            x,
            y,
            Some(w),
        );
        let bh = f64::from(tr.measure(&commit.body, 13.0, Some(w)).1);
        y + bh + 18.0
    }

    /// The changed-files list, if any (part of [`Self::draw_detail`]).
    #[allow(clippy::too_many_arguments)]
    fn draw_detail_files(
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        area: Rect,
        commit: &Commit,
        x: f64,
        mut y: f64,
        w: f32,
        pad: f64,
    ) {
        let c = &theme.tokens.colors;
        if commit.files.is_empty() {
            return;
        }
        draw::text_weighted(
            scene,
            tr,
            t,
            "Changed files",
            12.0,
            c.text_dim,
            x,
            y,
            None,
            TextWeight::Semibold,
        );
        y += 24.0;
        for f in &commit.files {
            let badge = f.kind.badge();
            draw::mono(
                scene,
                tr,
                t,
                badge,
                12.0,
                draw::kind_color(theme, f.kind),
                x,
                y,
            );
            draw::text(
                scene,
                tr,
                t,
                &f.path,
                12.0,
                c.text_primary,
                x + 18.0,
                y,
                Some(w - 80.0),
            );
            let counts = format!("+{} -{}", f.additions, f.deletions);
            let cw = f64::from(draw::mono_width(tr, &counts, 10.5));
            draw::mono(
                scene,
                tr,
                t,
                &counts,
                10.5,
                c.text_dim,
                area.x1 - pad - cw,
                y + 1.0,
            );
            y += 24.0;
        }
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "internal draw-step split of an over-length function"
)]
fn meta_row(
    scene: &mut Scene,
    tr: &mut TextRenderer,
    t: Affine,
    theme: &Theme,
    x: f64,
    y: f64,
    _w: f32,
    label: &str,
    value: &str,
    mono: bool,
) -> f64 {
    let c = &theme.tokens.colors;
    draw::text(scene, tr, t, label, 11.5, c.text_dim, x, y, None);
    if mono {
        draw::mono(scene, tr, t, value, 12.0, c.text_primary, x + 70.0, y);
    } else {
        draw::text(scene, tr, t, value, 12.0, c.text_primary, x + 70.0, y, None);
    }
    y + 22.0
}

fn ref_colors(theme: &Theme, kind: RefKind) -> (Color, Color) {
    let c = &theme.tokens.colors;
    match kind {
        RefKind::Head => (Color::WHITE, theme.accent_primary),
        RefKind::LocalBranch => (c.text_primary, c.accent[2]),
        RefKind::RemoteBranch => {
            let g = c.semantic.success;
            (Color::WHITE, Color::from_rgba8(g.r(), g.g(), g.b(), 60))
        }
        RefKind::Tag => {
            let a = c.semantic.warning;
            (c.surfaces.base, Color::from_rgba8(a.r(), a.g(), a.b(), 220))
        }
    }
}
