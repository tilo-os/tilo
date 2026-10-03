//! The history screen: the commit list with its branch graph, and the
//! selected commit.
//!
//! The list stands on the sidebar region. Every commit row has the same two
//! lines: the summary with its age, then the refs that point at it (as
//! chips) with the author, and the short hash at the trailing edge. The
//! graph is content the app draws: lanes and dots in the theme's series
//! colours. The detail is a readable column on the window ground: the
//! summary as the title, the author, the commit's facts as a property list,
//! the message, and the files it changed.

use vello::kurbo::{BezPath, Point, Rect, Stroke};

use ui::chrome::{SelectionState, paint_row_selection};
use ui::paint::PaintCtx;
use ui::text_render::{TextKind, TextRenderer, TextStyle};
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::widget::{
    AvatarFace, BadgeFace, ChipFace, ChipLead, ListRowFace, PropertyListFace, PropertyRow,
    RowLeading, badge_column_width,
};

use crate::changes::line_counts;
use crate::graph::GraphLayout;
use crate::model::{Commit, RefKind, RefLabel, RepoModel};

/// Row geometry of the commit list, measured from the theme.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Metrics {
    /// One commit row (both lines and their padding).
    pub row_h: f64,
    pad_y: f64,
    line_gap: f64,
    summary_h: f64,
    chip_h: f64,
    /// The list's inset from the column edges.
    inset: f64,
    /// Horizontal padding inside a row.
    pad_x: f64,
    /// Distance between two graph lanes.
    lane_w: f64,
    dot_r: f64,
}

impl Metrics {
    pub fn measure(text: &mut TextRenderer, theme: &Theme) -> Self {
        let s = &theme.tokens.spacing;
        let summary_h = f64::from(
            text.measure_style("Ag", None, &theme.text(TextRole::Body))
                .1,
        );
        let chip_h = ChipFace::height(theme);
        let pad_y = f64::from(s.s2);
        let line_gap = f64::from(s.s1);
        Self {
            row_h: (pad_y * 2.0 + summary_h + line_gap + chip_h).ceil(),
            pad_y,
            line_gap,
            summary_h,
            chip_h,
            inset: f64::from(s.s2),
            pad_x: f64::from(s.s3),
            lane_w: f64::from(s.s4),
            dot_r: f64::from(s.s1),
        }
    }
}

/// The commit list's input for one frame.
pub struct ListInput<'a> {
    pub model: &'a RepoModel,
    pub layout: &'a GraphLayout,
    pub selected: usize,
    pub scroll: f64,
    pub pointer: Option<Point>,
}

/// What painting the list produced.
pub struct ListDraw {
    /// Each visible commit's row, by index into the commit list.
    pub rows: Vec<(usize, Rect)>,
    pub metrics: Metrics,
    /// Height of all rows, for clamping the scroll.
    pub content_height: f64,
}

/// The age of a commit at `time`, read off the toolkit clock.
pub fn age(time: i64) -> String {
    let now = ui::clock::local_now().timestamp();
    ui::format::relative(now.saturating_sub(time))
}

/// A commit's date and time in the local zone: `30 June 2026, 08:12`.
pub fn date(time: i64) -> String {
    use chrono::{Datelike, Timelike};
    let Some(utc) = chrono::DateTime::from_timestamp(time, 0) else {
        return String::new();
    };
    let local = utc.with_timezone(&chrono::Local);
    format!(
        "{} {} {}, {:02}:{:02}",
        local.day(),
        ui::format::month_name(local.month0() as usize),
        local.year(),
        local.hour(),
        local.minute()
    )
}

/// The first seven characters of a commit id.
pub fn short_id(id: &str) -> &str {
    id.get(..7).unwrap_or(id)
}

/// Paint the commit list into `area` (on the sidebar region).
pub fn paint_list(ctx: &mut PaintCtx<'_>, area: Rect, input: &ListInput<'_>) -> ListDraw {
    let m = Metrics::measure(ctx.text, ctx.theme);
    let top = area.y0 + m.inset - input.scroll;
    let count = input.model.commits.len();
    let content_height = count as f64 * m.row_h + 2.0 * m.inset;
    ctx.scene.push_layer(
        vello::peniko::Fill::NonZero,
        vello::peniko::BlendMode::default(),
        1.0,
        ctx.transform,
        &area,
    );
    let row_at = |i: usize| {
        Rect::new(
            area.x0 + m.inset,
            top + i as f64 * m.row_h,
            area.x1 - m.inset,
            top + (i + 1) as f64 * m.row_h,
        )
    };
    let pointer = input.pointer.filter(|p| area.contains(*p));
    let ground = ctx.theme.tokens.colors.surfaces.content;
    let mut rows = Vec::new();
    // Selection and hover sit under the graph, which runs through every row.
    for i in 0..count {
        let row = row_at(i);
        if row.y1 < area.y0 || row.y0 > area.y1 {
            continue;
        }
        let hovered = pointer.is_some_and(|p| row.contains(p));
        paint_row_selection(
            ctx.scene,
            ctx.transform,
            row,
            SelectionState::new(i == input.selected, hovered, false),
            ground,
            ctx.theme,
        );
        rows.push((i, row));
    }
    let graph_x = area.x0 + m.inset + m.pad_x;
    let lanes = input.layout.lanes.max(1) as f64;
    paint_graph(ctx, input, &m, graph_x, &row_at, area);
    let text_x = graph_x + lanes * m.lane_w + f64::from(ctx.theme.tokens.spacing.s2);
    for &(i, row) in &rows {
        if let Some(commit) = input.model.commits.get(i) {
            paint_row_text(ctx, commit, row, text_x, &m, &input.model.head);
        }
    }
    ctx.scene.pop_layer();
    ListDraw {
        rows,
        metrics: m,
        content_height,
    }
}

/// The lane edges and the commit dots, in the theme's series colours.
fn paint_graph(
    ctx: &mut PaintCtx<'_>,
    input: &ListInput<'_>,
    m: &Metrics,
    graph_x: f64,
    row_at: &dyn Fn(usize) -> Rect,
    area: Rect,
) {
    let theme = ctx.theme;
    let colors = &theme.tokens.colors;
    let lane_x = |lane: usize| graph_x + lane as f64 * m.lane_w + m.lane_w / 2.0;
    let centre_y = |row: usize| row_at(row).center().y;
    let stroke = Stroke::new(2.0 * f64::from(theme.ui_scale));
    for edge in &input.layout.edges {
        let (y0, y1) = (centre_y(edge.from_row), centre_y(edge.to_row));
        if y0.max(y1) < area.y0 - m.row_h || y0.min(y1) > area.y1 + m.row_h {
            continue;
        }
        let (x0, x1) = (lane_x(edge.from_lane), lane_x(edge.to_lane));
        let mut path = BezPath::new();
        path.move_to(Point::new(x0, y0));
        if (x0 - x1).abs() < 0.5 {
            path.line_to(Point::new(x1, y1));
        } else if x1 > x0 {
            // A second parent branches off: bend out of the row, then run
            // down the parent's lane.
            let bend = (y0 + m.row_h).min(y1);
            let mid = f64::midpoint(y0, bend);
            path.curve_to(
                Point::new(x0, mid),
                Point::new(x1, mid),
                Point::new(x1, bend),
            );
            path.line_to(Point::new(x1, y1));
        } else {
            // A lane joins its parent: run down the lane, then bend in.
            let bend = (y1 - m.row_h).max(y0);
            let mid = f64::midpoint(bend, y1);
            path.line_to(Point::new(x0, bend));
            path.curve_to(Point::new(x0, mid), Point::new(x1, mid), Point::new(x1, y1));
        }
        let colour = colors.presence.series(edge.to_lane.max(edge.from_lane));
        ctx.scene
            .stroke(&stroke, ctx.transform, colour, None, &path);
    }
    for (row, commit) in input.model.commits.iter().enumerate() {
        let y = centre_y(row);
        if y < area.y0 - m.row_h || y > area.y1 + m.row_h {
            continue;
        }
        let lane = input.layout.lane_of.get(row).copied().unwrap_or(0);
        let centre = Point::new(lane_x(lane), y);
        let colour = colors.presence.series(lane);
        let r = m.dot_r + stroke.width / 2.0;
        let dot = Rect::from_center_size(centre, (2.0 * r, 2.0 * r));
        if commit.parents.len() > 1 {
            // A merge is a ring.
            ctx.stroke_round(dot, r, colour, stroke.width);
        } else {
            ctx.fill_round(dot, r, colour);
        }
    }
}

/// One commit row's text: summary and age, then the refs, the author and
/// the short hash.
fn paint_row_text(
    ctx: &mut PaintCtx<'_>,
    commit: &Commit,
    row: Rect,
    text_x: f64,
    m: &Metrics,
    head: &str,
) {
    let theme = ctx.theme;
    let gap = f64::from(theme.tokens.spacing.s2);
    let right = row.x1 - m.pad_x;
    let line1 = Rect::new(
        text_x,
        row.y0 + m.pad_y,
        right,
        row.y0 + m.pad_y + m.summary_h,
    );
    let line2 = Rect::new(
        text_x,
        line1.y1 + m.line_gap,
        right,
        line1.y1 + m.line_gap + m.chip_h,
    );

    let meta = theme.text(TextRole::Caption).numeric();
    let age = age(commit.time);
    let (age_w, age_h) = ctx.text.measure_style(&age, None, &meta);
    let age_x = right - f64::from(age_w);
    ctx.text(
        &age,
        &meta,
        theme.text_dim,
        Point::new(
            age_x,
            ui::text_metrics::centered_text_top(line1.y0, line1.height(), f64::from(age_h)),
        ),
        None,
    );
    let body = theme.text(TextRole::Body);
    let summary = ctx.text.elide_to_width_style(
        &commit.summary,
        ui::num::f32_of_f64((age_x - gap - text_x).max(0.0)),
        &body,
    );
    ctx.text_role_in_band(
        &summary,
        TextRole::Body,
        theme.text_primary,
        text_x,
        line1,
        None,
    );

    let hash_style = hash_style(theme);
    let hash = short_id(&commit.id);
    let (hash_w, hash_h) = ctx.text.measure_style(hash, None, &hash_style);
    let hash_x = right - f64::from(hash_w);
    ctx.text(
        hash,
        &hash_style,
        theme.text_tertiary,
        Point::new(
            hash_x,
            ui::text_metrics::centered_text_top(line2.y0, line2.height(), f64::from(hash_h)),
        ),
        None,
    );
    let limit = hash_x - gap;
    let mut x = text_x;
    for label in shown_refs(&commit.refs) {
        let lead = ref_lead(theme, label, head);
        let w = ChipFace::intrinsic_width(&label.name, lead, false, ctx.text, theme);
        let room = limit - x;
        if room <= m.chip_h {
            break;
        }
        let chip = Rect::new(x, line2.y0, x + w.min(room), line2.y1);
        ChipFace::new(chip, &label.name).lead(lead).paint(
            ctx.scene,
            ctx.transform,
            ctx.text,
            theme,
        );
        x = chip.x1 + gap;
        if w > room {
            break;
        }
    }
    // The author shows whole or not at all: the detail names them anyway.
    let caption = theme.text(TextRole::Caption);
    let author_w = f64::from(
        ctx.text
            .measure_style(&commit.author_name, None, &caption)
            .0,
    );
    if x + author_w <= limit {
        ctx.text_role_in_band(
            &commit.author_name,
            TextRole::Caption,
            theme.text_dim,
            x,
            line2,
            None,
        );
    }
}

/// Hashes: `Code` with tabular figures.
fn hash_style(theme: &Theme) -> TextStyle {
    theme.text(TextRole::Code).numeric()
}

/// The refs a row shows: everything but the bare `HEAD` marker, which the
/// checked-out branch's chip carries instead.
fn shown_refs(refs: &[RefLabel]) -> impl Iterator<Item = &RefLabel> {
    refs.iter().filter(|r| r.kind != RefKind::Head)
}

/// The checked-out branch is where you are: its chip leads with a dot in
/// your colour. Every other ref is a plain chip.
fn ref_lead(theme: &Theme, label: &RefLabel, head: &str) -> ChipLead {
    if label.kind == RefKind::LocalBranch && label.name == head {
        ChipLead::Dot(theme.tokens.colors.intent.you)
    } else {
        ChipLead::None
    }
}

/// A commit message body reflowed for reading: single line breaks inside a
/// paragraph (git's hard wrap) become spaces; blank lines stay paragraph
/// breaks.
pub fn reflow(body: &str) -> String {
    body.split("\n\n")
        .map(|para| {
            para.lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .collect::<Vec<_>>()
                .join(" ")
        })
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Paint the selected commit into `area`, scrolled by `scroll`. Returns the
/// height of everything, for clamping the scroll.
pub fn paint_detail(ctx: &mut PaintCtx<'_>, area: Rect, commit: &Commit, scroll: f64) -> f64 {
    let theme = ctx.theme;
    let s = &theme.tokens.spacing;
    let gutter = f64::from(s.s6);
    let width =
        (area.width() - 2.0 * gutter).clamp(0.0, ui::content_layout::FOCUSED_CONTENT_MAX_WIDTH);
    let x = area.x0 + gutter;
    let w32 = ui::num::f32_of_f64(width);
    let top = area.y0 + gutter - scroll;
    let mut y = top;
    ctx.scene.push_layer(
        vello::peniko::Fill::NonZero,
        vello::peniko::BlendMode::default(),
        1.0,
        ctx.transform,
        &area,
    );

    // The summary is the detail's title.
    let title = theme.text(TextRole::Title);
    let (_, title_h) = ctx.text.measure_style(&commit.summary, Some(w32), &title);
    ctx.text(
        &commit.summary,
        &title,
        theme.text_primary,
        Point::new(x, y),
        Some(w32),
    );
    y += f64::from(title_h) + f64::from(s.s4);

    // The author.
    let avatar = ui::widget::AvatarSize::Regular.side(theme);
    AvatarFace::new(&commit.author_name, &commit.author_email)
        .paint(ctx, Rect::new(x, y, x + avatar, y + avatar));
    let name_style = theme.text(TextRole::Body).strong();
    let mail_style = theme
        .text(TextRole::Caption)
        .with_kind(TextKind::Identifier);
    let name_h = f64::from(ctx.text.measure_style("Ag", None, &name_style).1);
    let mail_h = f64::from(ctx.text.measure_style("Ag", None, &mail_style).1);
    let text_x = x + avatar + f64::from(s.s3);
    let text_top = y + (avatar - name_h - mail_h) / 2.0;
    ctx.text(
        &commit.author_name,
        &name_style,
        theme.text_primary,
        Point::new(text_x, text_top),
        None,
    );
    ctx.text(
        &commit.author_email,
        &mail_style,
        theme.text_dim,
        Point::new(text_x, text_top + name_h),
        None,
    );
    y += avatar + f64::from(s.s6);

    // Its facts.
    let date = date(commit.time);
    let parents = commit
        .parents
        .iter()
        .map(|p| short_id(p))
        .collect::<Vec<_>>()
        .join(", ");
    let refs = shown_refs(&commit.refs)
        .map(|r| r.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let mut facts = vec![
        PropertyRow::new("Commit", &commit.id).with_kind(TextKind::Secret),
        PropertyRow::new("Date", &date).with_kind(TextKind::Numeric),
    ];
    if !parents.is_empty() {
        let label = if commit.parents.len() > 1 {
            "Parents"
        } else {
            "Parent"
        };
        facts.push(PropertyRow::new(label, &parents).with_kind(TextKind::Secret));
    }
    if !refs.is_empty() {
        facts.push(PropertyRow::new("Branches and tags", &refs));
    }
    let list = PropertyListFace::new(&facts);
    let facts_h = list.height(ctx.text, theme, width);
    list.paint(ctx, Rect::new(x, y, x + width, y + facts_h));
    y += facts_h + f64::from(s.s6);

    // The message.
    let body = reflow(&commit.body);
    if !body.is_empty() {
        let style = theme.text(TextRole::Body);
        let (_, body_h) = ctx.text.measure_style(&body, Some(w32), &style);
        ctx.text(
            &body,
            &style,
            theme.text_primary,
            Point::new(x, y),
            Some(w32),
        );
        y += f64::from(body_h) + f64::from(s.s6);
    }

    // The files it changed.
    if !commit.files.is_empty() {
        y = paint_files(ctx, commit, Rect::new(x, y, x + width, area.y1), y);
    }
    ctx.scene.pop_layer();
    y - top + gutter
}

/// The "Changed files" group: a `Subheading` with its count over one row
/// per file. Returns the `y` under it.
fn paint_files(ctx: &mut PaintCtx<'_>, commit: &Commit, lane: Rect, y: f64) -> f64 {
    let theme = ctx.theme;
    let s = &theme.tokens.spacing;
    let pad = f64::from(s.s3);
    let header_h = f64::from(
        ctx.text
            .measure_style("Ag", None, &theme.text(TextRole::Subheading))
            .1,
    )
    .max(BadgeFace::height(theme));
    let band = Rect::new(lane.x0, y, lane.x1, y + header_h);
    let title = "Changed files";
    let title_w = f64::from(
        ctx.text
            .measure_style(title, None, &theme.text(TextRole::Subheading))
            .0,
    );
    ctx.text_role_in_band(
        title,
        TextRole::Subheading,
        theme.text_primary,
        band.x0,
        band,
        None,
    );
    let count = commit.files.len().to_string();
    let count_w = BadgeFace::intrinsic_width(&count, ctx.text, theme);
    let badge_h = BadgeFace::height(theme);
    BadgeFace::new(
        Rect::from_origin_size(
            Point::new(
                band.x0 + title_w + f64::from(s.s2),
                band.center().y - badge_h / 2.0,
            ),
            (count_w, badge_h),
        ),
        &count,
    )
    .paint(ctx.scene, ctx.transform, ctx.text, theme);

    let badges: Vec<&str> = commit.files.iter().map(|f| f.kind.badge()).collect();
    let badge_w = badge_column_width(ctx.text, theme, &badges);
    // Rows run from the lane's edges, so their text lines up with the
    // column's other text.
    let mut y = band.y1 + f64::from(s.s2);
    let count = commit.files.len();
    for (i, file) in commit.files.iter().enumerate() {
        let counts = line_counts(file.additions, file.deletions);
        let dir = file.dir();
        let mut face = ListRowFace::new(file.name())
            .leading(RowLeading::Badge(file.kind.badge()))
            .leading_column(badge_w);
        if !dir.is_empty() {
            face = face.secondary(dir);
        }
        if let Some(counts) = counts.as_deref() {
            face = face.meta(counts);
        }
        if i + 1 < count {
            face = face.separator(false);
        }
        let h = face.height(ctx.text, theme);
        let rect = Rect::new(lane.x0 - pad, y, lane.x1 + pad, y + h);
        face.paint(ctx, rect);
        y += h;
    }
    y
}

#[cfg(test)]
mod tests {
    use super::*;
    use vello::Scene;
    use vello::kurbo::Affine;

    /// Every commit row has the same height, grown with the text scale, and
    /// the list reports a row for every visible commit.
    #[test]
    fn commit_rows_are_one_height_and_follow_the_text_scale() {
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let one = Metrics::measure(&mut text, &theme);
        let two = Metrics::measure(&mut text, &Theme::dark().with_text_scale(2.0));
        assert!(two.row_h > one.row_h);

        let (mut model, _) = crate::sample::model();
        let layout = crate::graph::layout(&mut model.commits);
        let mut text = TextRenderer::new();
        let mut scene = Scene::new();
        let mut ctx = PaintCtx::new(&mut scene, Affine::IDENTITY, &mut text, &theme);
        let out = paint_list(
            &mut ctx,
            Rect::new(0.0, 0.0, 340.0, 2000.0),
            &ListInput {
                model: &model,
                layout: &layout,
                selected: 0,
                scroll: 0.0,
                pointer: None,
            },
        );
        assert_eq!(out.rows.len(), model.commits.len());
        for pair in out.rows.windows(2) {
            let [(_, a), (_, b)] = pair else { continue };
            assert!((a.height() - b.height()).abs() < 0.01);
            assert!((b.y0 - a.y1).abs() < 0.01, "rows stack edge to edge");
        }
    }

    #[test]
    fn a_hard_wrapped_body_reflows_into_paragraphs() {
        assert_eq!(
            reflow("Bring the pipeline\nonto main.\n\nReplaces the blit\npath."),
            "Bring the pipeline onto main.\n\nReplaces the blit path."
        );
        assert_eq!(reflow(""), "");
    }
}
