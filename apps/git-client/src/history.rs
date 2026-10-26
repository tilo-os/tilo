//! The history screen: the commit list with its branch graph, and the
//! selected commit.
//!
//! The list stands on the sidebar region. Every commit row has the same
//! three lines in the same places: the summary across the whole row; the
//! author with the commit's age at the trailing edge; and the short hash,
//! followed by the refs that point at the commit as chips. The
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
use crate::i18n::{t, t_args};
use crate::model::{Commit, RefKind, RefLabel, RepoModel};

/// Row geometry of the commit list, measured from the theme.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Metrics {
    /// One commit row (all three lines and their padding).
    pub row_h: f64,
    pad_y: f64,
    line_gap: f64,
    summary_h: f64,
    caption_h: f64,
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
        let caption_h = f64::from(
            text.measure_style("Ag", None, &theme.text(TextRole::Caption))
                .1,
        );
        let chip_h = ChipFace::height(theme);
        let pad_y = f64::from(s.s2);
        let line_gap = f64::from(s.s1);
        Self {
            row_h: (pad_y * 2.0 + summary_h + caption_h + line_gap + chip_h).ceil(),
            pad_y,
            line_gap,
            summary_h,
            caption_h,
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
    /// Height of all rows, for clamping the scroll.
    pub content_height: f64,
}

/// The age of a commit at `time`, read off the toolkit clock.
pub fn age(time: i64) -> String {
    let now = ui::clock::local_now().timestamp();
    ui::format::relative(now.saturating_sub(time))
}

/// A commit's date and time in the local zone, in the active locale:
/// `30 June 2026, 08:12`.
pub fn date(time: i64) -> String {
    use chrono::Datelike;
    let Some(utc) = chrono::DateTime::from_timestamp(time, 0) else {
        return String::new();
    };
    let local = utc.with_timezone(&chrono::Local);
    t_args(
        "git-date",
        &[
            ("day", local.day().to_string().into()),
            ("month", local.month().into()),
            ("year", local.year().to_string().into()),
            ("time", ui::clock::time_of_day(&local).into()),
        ],
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
    paint_graph(ctx, input, &m, graph_x, &row_at, area);
    let text_x = text_left(area, input.layout.lanes, &m, ctx.theme);
    for &(i, row) in &rows {
        if let Some(commit) = input.model.commits.get(i) {
            paint_row_text(ctx, commit, row, text_x, &m, &input.model.head);
        }
    }
    ctx.scene.pop_layer();
    ListDraw {
        rows,
        content_height,
    }
}

/// Where a row's text starts in a list over `area`: past the graph's
/// `lanes` and a gap. The text ends `pad_x` inside the row.
fn text_left(area: Rect, lanes: usize, m: &Metrics, theme: &Theme) -> f64 {
    area.x0
        + m.inset
        + m.pad_x
        + ui::num::f64_of_usize(lanes.max(1)) * m.lane_w
        + f64::from(theme.tokens.spacing.s2)
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

/// One commit row's text: the summary; the author and the age; the short
/// hash and the refs.
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
    let line2 = Rect::new(text_x, line1.y1, right, line1.y1 + m.caption_h);
    let line3 = Rect::new(
        text_x,
        line2.y1 + m.line_gap,
        right,
        line2.y1 + m.line_gap + m.chip_h,
    );

    // The summary has the whole line.
    let body = theme.text(TextRole::Body);
    let summary = ctx.text.elide_to_width_style(
        &commit.summary,
        ui::num::f32_of_f64(line1.width().max(0.0)),
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

    // The author, and the age at the trailing edge.
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
            ui::text_metrics::centered_text_top(line2.y0, line2.height(), f64::from(age_h)),
        ),
        None,
    );
    let caption = theme.text(TextRole::Caption);
    let author = ctx.text.elide_to_width_style(
        &commit.author_name,
        ui::num::f32_of_f64((age_x - gap - text_x).max(0.0)),
        &caption,
    );
    ctx.text_role_in_band(
        &author,
        TextRole::Caption,
        theme.text_dim,
        text_x,
        line2,
        None,
    );

    // The short hash, then the refs.
    let hash_style = hash_style(theme);
    let hash = short_id(&commit.id);
    let (hash_w, hash_h) = ctx.text.measure_style(hash, None, &hash_style);
    ctx.text(
        hash,
        &hash_style,
        theme.text_tertiary,
        Point::new(
            text_x,
            ui::text_metrics::centered_text_top(line3.y0, line3.height(), f64::from(hash_h)),
        ),
        None,
    );
    let line3 = Rect::new(text_x + f64::from(hash_w) + gap, line3.y0, right, line3.y1);
    paint_refs(ctx, commit, line3, head);
}

/// The refs pointing at a row's commit as chips in `line`, as many as fit
/// whole and a count chip for the rest.
fn paint_refs(ctx: &mut PaintCtx<'_>, commit: &Commit, line: Rect, head: &str) {
    let theme = ctx.theme;
    let gap = f64::from(theme.tokens.spacing.s2);
    let x = line.x0;
    let refs: Vec<&RefLabel> = shown_refs(&commit.refs).collect();
    let leads: Vec<ChipLead> = refs
        .iter()
        .map(|label| ref_lead(theme, label, head))
        .collect();
    let widths: Vec<f64> = refs
        .iter()
        .zip(&leads)
        .map(|(label, lead)| ChipFace::intrinsic_width(&label.name, *lead, false, ctx.text, theme))
        .collect();
    let plan = plan_chips(&widths, line.x1 - x, gap, |hidden| {
        ChipFace::intrinsic_width(&more_label(hidden), ChipLead::None, false, ctx.text, theme)
    });
    let mut x = x;
    for ((label, lead), w) in refs.iter().zip(&leads).zip(&plan.widths) {
        let chip = Rect::new(x, line.y0, x + w, line.y1);
        ChipFace::new(chip, &label.name).lead(*lead).paint(
            ctx.scene,
            ctx.transform,
            ctx.text,
            theme,
        );
        x = chip.x1 + gap;
    }
    if let Some(hidden) = plan.hidden {
        let label = more_label(hidden);
        let w = ChipFace::intrinsic_width(&label, ChipLead::None, false, ctx.text, theme);
        ChipFace::new(Rect::new(x, line.y0, x + w, line.y1), &label).paint(
            ctx.scene,
            ctx.transform,
            ctx.text,
            theme,
        );
    }
}

/// The chip that stands for the refs a row has no room for.
fn more_label(hidden: usize) -> String {
    t_args("git-more-refs", &[("count", hidden.into())])
}

/// Which of a row's ref chips stand whole in `room`, and how many a
/// trailing count chip stands for.
#[derive(Debug, PartialEq)]
struct ChipPlan {
    /// The width of each chip shown, in order; only the first is ever
    /// narrower than it would be, elided when nothing else fits.
    widths: Vec<f64>,
    hidden: Option<usize>,
}

/// Lay out chips of `widths` with `gap` between them in `room`: as many as
/// fit whole, then a count chip (`more_width` of the hidden count) for the
/// rest. When not even the first fits beside the count, the first is elided
/// to what is left, never below a minimum that still reads.
fn plan_chips(
    widths: &[f64],
    room: f64,
    gap: f64,
    mut more_width: impl FnMut(usize) -> f64,
) -> ChipPlan {
    let total = |k: usize| widths.iter().take(k).sum::<f64>() + gap * k.saturating_sub(1) as f64;
    if total(widths.len()) <= room {
        return ChipPlan {
            widths: widths.to_vec(),
            hidden: None,
        };
    }
    for shown in (1..widths.len()).rev() {
        if total(shown) + gap + more_width(widths.len() - shown) <= room {
            return ChipPlan {
                widths: widths.iter().take(shown).copied().collect(),
                hidden: Some(widths.len() - shown),
            };
        }
    }
    let Some(first) = widths.first() else {
        return ChipPlan {
            widths: Vec::new(),
            hidden: None,
        };
    };
    let rest = widths.len() - 1;
    let count = if rest > 0 {
        gap + more_width(rest)
    } else {
        0.0
    };
    let elided = (room - count).min(*first);
    let readable = 3.0 * gap;
    if elided >= readable {
        ChipPlan {
            widths: vec![elided],
            hidden: (rest > 0).then_some(rest),
        }
    } else if more_width(widths.len()) <= room {
        ChipPlan {
            widths: Vec::new(),
            hidden: Some(widths.len()),
        }
    } else {
        ChipPlan {
            widths: Vec::new(),
            hidden: None,
        }
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

/// A commit's facts for its property list, in the active locale.
struct Facts {
    commit_label: String,
    id: String,
    date_label: String,
    date: String,
    parents_label: String,
    parents: String,
    refs_label: String,
    refs: String,
}

impl Facts {
    fn of(commit: &Commit) -> Self {
        Self {
            commit_label: t("git-fact-commit"),
            id: commit.id.clone(),
            date_label: t("git-fact-date"),
            date: date(commit.time),
            parents_label: t_args(
                "git-fact-parents",
                &[("count", commit.parents.len().into())],
            ),
            parents: commit
                .parents
                .iter()
                .map(|p| short_id(p))
                .collect::<Vec<_>>()
                .join(", "),
            refs_label: t("git-fact-refs"),
            refs: shown_refs(&commit.refs)
                .map(|r| r.name.as_str())
                .collect::<Vec<_>>()
                .join(", "),
        }
    }

    /// The rows: the id, the date, the parents and the refs pointing here
    /// (each of the last two only when there are any).
    fn rows(&self) -> Vec<PropertyRow<'_>> {
        let mut rows = vec![
            PropertyRow::new(&self.commit_label, &self.id).with_kind(TextKind::Secret),
            PropertyRow::new(&self.date_label, &self.date).with_kind(TextKind::Numeric),
        ];
        if !self.parents.is_empty() {
            rows.push(
                PropertyRow::new(&self.parents_label, &self.parents).with_kind(TextKind::Secret),
            );
        }
        if !self.refs.is_empty() {
            // Branch names never break: an `-` inside one is no word end.
            rows.push(
                PropertyRow::new(&self.refs_label, &self.refs).with_kind(TextKind::Identifier),
            );
        }
        rows
    }
}

/// What one detail paint produced.
pub struct DetailDraw {
    /// Height of everything, for clamping the scroll.
    pub height: f64,
    /// The changed-file rows, top to bottom, at their painted places: what
    /// the pointer and the keyboard choose to see that file's diff.
    pub file_rows: Vec<Rect>,
}

/// Paint the selected commit into `area`, scrolled by `scroll`.
pub fn paint_detail(
    ctx: &mut PaintCtx<'_>,
    area: Rect,
    commit: &Commit,
    scroll: f64,
) -> DetailDraw {
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
    let fact_text = Facts::of(commit);
    let facts = fact_text.rows();
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
    let file_rows = if commit.files.is_empty() {
        Vec::new()
    } else {
        let (next, rows) = paint_files(ctx, commit, Rect::new(x, y, x + width, area.y1), y);
        y = next;
        rows
    };
    ctx.scene.pop_layer();
    DetailDraw {
        height: y - top + gutter,
        file_rows,
    }
}

/// The "Changed files" group: a `Subheading` with its count over one row
/// per file. Returns the `y` under it and each row's rect.
fn paint_files(ctx: &mut PaintCtx<'_>, commit: &Commit, lane: Rect, y: f64) -> (f64, Vec<Rect>) {
    let theme = ctx.theme;
    let s = &theme.tokens.spacing;
    let pad = ui::chrome::row_bleed(theme);
    let header_h = f64::from(
        ctx.text
            .measure_style("Ag", None, &theme.text(TextRole::Subheading))
            .1,
    )
    .max(BadgeFace::height(theme));
    let band = Rect::new(lane.x0, y, lane.x1, y + header_h);
    let title = t("git-changed-files");
    let title_w = f64::from(
        ctx.text
            .measure_style(&title, None, &theme.text(TextRole::Subheading))
            .0,
    );
    ctx.text_role_in_band(
        &title,
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
    let mut rows = Vec::with_capacity(count);
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
        rows.push(rect);
        y += h;
    }
    (y, rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use vello::Scene;
    use vello::kurbo::Affine;

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

    /// At the list's width, docked or as a drawer, the bundled sample shows
    /// every summary, author and ref whole: nothing in the shipped
    /// repository is elided.
    #[test]
    fn the_sample_rows_fit_the_list_whole() {
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let m = Metrics::measure(&mut text, &theme);
        let (mut model, _) = crate::sample::model();
        let layout = crate::graph::layout(&mut model.commits);
        let width = f64::from(theme.tokens.metrics.overview_width);
        let area = Rect::new(0.0, 0.0, width, 800.0);
        let left = text_left(area, layout.lanes, &m, &theme);
        let room = area.x1 - m.inset - m.pad_x - left;
        let gap = f64::from(theme.tokens.spacing.s2);
        let width_of = |text: &mut TextRenderer, s: &str, style: &TextStyle| {
            f64::from(text.measure_style(s, None, style).0)
        };
        let body = theme.text(TextRole::Body);
        let caption = theme.text(TextRole::Caption);
        let meta = theme.text(TextRole::Caption).numeric();
        for commit in &model.commits {
            assert!(
                width_of(&mut text, &commit.summary, &body) <= room,
                "{:?} fits its line",
                commit.summary
            );
            let line2 = width_of(&mut text, &commit.author_name, &caption)
                + gap
                + width_of(&mut text, &age(commit.time), &meta);
            assert!(line2 <= room, "{:?}: author and age fit", commit.summary);
            let mut line3 = width_of(&mut text, short_id(&commit.id), &hash_style(&theme));
            for label in shown_refs(&commit.refs) {
                let lead = ref_lead(&theme, label, &model.head);
                line3 +=
                    gap + ChipFace::intrinsic_width(&label.name, lead, false, &mut text, &theme);
            }
            assert!(line3 <= room, "{:?}: hash and refs fit", commit.summary);
        }
    }

    #[test]
    fn chips_that_do_not_fit_are_counted_in_a_chip_of_their_own() {
        let more = |hidden: usize| 20.0 + hidden as f64;
        let widths = [40.0, 80.0, 120.0];
        assert_eq!(
            plan_chips(&widths, 260.0, 8.0, more),
            ChipPlan {
                widths: widths.to_vec(),
                hidden: None
            },
            "all three fit"
        );
        assert_eq!(
            plan_chips(&widths, 160.0, 8.0, more),
            ChipPlan {
                widths: vec![40.0, 80.0],
                hidden: Some(1)
            },
            "the third is counted, never dropped"
        );
        assert_eq!(
            plan_chips(&widths, 100.0, 8.0, more),
            ChipPlan {
                widths: vec![40.0],
                hidden: Some(2)
            }
        );
        let elided = plan_chips(&[200.0, 50.0], 120.0, 8.0, more);
        assert_eq!(elided.hidden, Some(1), "the rest still counted");
        assert!(elided.widths.len() == 1 && elided.widths[0] < 200.0);
        assert!(elided.widths[0] + 8.0 + more(1) <= 120.0);
        assert_eq!(
            plan_chips(&[200.0, 50.0], 30.0, 8.0, more),
            ChipPlan {
                widths: Vec::new(),
                hidden: Some(2)
            },
            "with room for the count alone, the count"
        );
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
