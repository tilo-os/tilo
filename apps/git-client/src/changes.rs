//! The changes column: staged / unstaged / untracked file lists with
//! per-row stage / unstage affordances and a commit box at the bottom.
//!
//! Rendered into a fixed-width left column of the changes screen. Each
//! row is hit-testable (select the file → show its diff; click the
//! stage/unstage chevron → move it between areas). The commit box shows
//! the author identity and a commit button whose enabled state tracks the
//! staged count and message length.

use vello::kurbo::{Point, Rect};

use ui::paint::PaintCtx;
use ui::text_render::{TextRenderer, TextStyle, TextWeight};
use ui::theme::Theme;
use ui::widget::ButtonFace;

use crate::draw;
use crate::model::{ChangeArea, FileChange, RepoModel};

/// 1× band heights. The painted bands are [`Metrics`]: these values while the
/// text fits, the measured line plus its padding once the text scale outgrows
/// them, so every value here is both the floor and the exact 1× result.
const ROW_H: f64 = 30.0;
const SECTION_H: f64 = 30.0;
const COMMIT_BOX_H: f64 = 150.0;
/// Height of the "— nothing here —" band an empty section paints.
const EMPTY_HINT_H: f64 = 22.0;
/// Left/right inset of section titles and bulk actions.
const GUTTER: f64 = 14.0;
/// Chevron button: square-ish, inset from the row's right edge.
const CHEVRON_W: f64 = 20.0;
const CHEVRON_INSET: f64 = 8.0;
/// Gap between the +/- counts and the chevron button.
const COUNTS_GAP: f64 = 8.0;
/// Fractional text sizes painted in the column (whole-number sizes stay inline
/// at their call sites: 12 px titles/hints/commit label, 11 px bulk action,
/// 10 px count badge).
const NAME_FONT: f32 = 12.5;
const AUTHOR_FONT: f32 = 11.5;

/// Band metrics measured from the renderer the column paints with.
///
/// Refreshed at the top of every draw; rows, section headers, the empty hint,
/// the commit box and every hit rect read this one snapshot, so a text scale
/// that grows the glyphs grows the bands with them. `Default` is the 1×
/// column as it has always been laid out: 30 px rows and section bands, a
/// 22 px empty hint, and a 150 px commit box of 12 pad + 22 author row + 10 +
/// 54 input + 10 + 30 button (138 wide) + 12.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Metrics {
    /// File row height.
    pub row_h: f64,
    /// Gap between neighbouring rows and under a section header (`s1`):
    /// HIG §7 — nothing touches, even in a dense list.
    pub row_gap: f64,
    /// Section header band.
    pub section_h: f64,
    /// "— nothing here —" band.
    pub empty_hint_h: f64,
    /// Author identity row inside the commit box.
    pub author_row_h: f64,
    /// Commit-message input height.
    pub input_h: f64,
    /// Commit button height and minimum width.
    pub button_h: f64,
    pub button_w: f64,
    /// Whole commit box pinned to the column's bottom.
    pub commit_box_h: f64,
    /// Outer padding of the commit box and the gap between its rows.
    pub commit_pad: f64,
    pub commit_gap: f64,
}

impl Default for Metrics {
    fn default() -> Self {
        Self {
            row_h: ROW_H,
            row_gap: 4.0,
            section_h: SECTION_H,
            empty_hint_h: EMPTY_HINT_H,
            author_row_h: 22.0,
            input_h: 54.0,
            button_h: 30.0,
            button_w: 138.0,
            commit_box_h: COMMIT_BOX_H,
            commit_pad: 12.0,
            commit_gap: 10.0,
        }
    }
}

impl Metrics {
    pub(crate) fn measure(tr: &mut TextRenderer, theme: &Theme) -> Self {
        let one = Self::default();
        let line = |tr: &mut TextRenderer, size: f32, weight: TextWeight| {
            f64::from(tr.measure_weighted("Ag", size, None, weight).1)
        };
        let name_h = line(tr, NAME_FONT, TextWeight::Medium);
        let title_h = line(tr, 12.0, TextWeight::Semibold);
        let hint_h = line(tr, 12.0, TextWeight::Regular);
        let author_h = line(tr, AUTHOR_FONT, TextWeight::Regular);
        let button_label_h = line(tr, 12.0, TextWeight::Semibold);
        // 6 px of leading either side of a name / title (1×: 15.6 + 12 ≤ 30);
        // an off-grid value the column has always used, so it stays local.
        let pad_y = 6.0;
        let row_h = (name_h + 2.0 * pad_y).max(one.row_h);
        let section_h = (title_h + 2.0 * pad_y).max(one.section_h);
        let empty_hint_h = (hint_h + pad_y).max(one.empty_hint_h);
        let author_row_h = author_h.max(one.author_row_h);
        // Two lines of the shared TextInput's 13 px body text plus its 21 px
        // frame (1×: 2 × 16.25 + 21 = 53.5 ≤ the 54 px floor).
        let input_h = (2.0 * line(tr, 13.0, TextWeight::Regular) + 21.0).max(one.input_h);
        let button_h = (button_label_h + 2.0 * pad_y).max(one.button_h);
        let commit_pad = f64::from(theme.tokens.spacing.s3);
        let row_gap = f64::from(theme.tokens.spacing.s1);
        let commit_box_h = (commit_pad
            + author_row_h
            + one.commit_gap
            + input_h
            + one.commit_gap
            + button_h
            + commit_pad)
            .max(one.commit_box_h);
        Self {
            row_h,
            row_gap,
            section_h,
            empty_hint_h,
            author_row_h,
            input_h,
            button_h,
            commit_box_h,
            commit_pad,
            ..one
        }
    }
}

/// Measured horizontal layout of one file row. Draw and any test consume the
/// same numbers, so the painted glyphs and the reserved widths agree.
pub(crate) struct RowLayout {
    pub badge: &'static str,
    pub badge_x: f64,
    pub name_x: f64,
    /// Width the file name may use before the dir / counts / chevron start.
    pub name_max_w: f64,
    pub dir_x: f64,
    /// Width left for the directory hint (0 when nothing fits).
    pub dir_max_w: f64,
    /// `+adds -dels`, absent when the file has no line counts.
    pub counts: Option<String>,
    pub counts_x: f64,
    pub chevron: &'static str,
    pub chevron_rect: Rect,
}

impl RowLayout {
    pub(crate) fn measure(
        tr: &mut TextRenderer,
        theme: &Theme,
        area: Rect,
        y: f64,
        row_h: f64,
        f: &FileChange,
        is_staged: bool,
    ) -> Self {
        let badge = f.kind.badge();
        let badge_x = area.x0 + GUTTER;
        let name_x = area.x0 + 32.0;
        // The chevron keeps its 1× insets (one spacing step above and below)
        // and widens in proportion to the row.
        let inset_y = f64::from(theme.tokens.spacing.s1);
        let chevron_w = CHEVRON_W * row_h / ROW_H;
        let chevron_rect = Rect::new(
            area.x1 - CHEVRON_INSET - chevron_w,
            y + inset_y,
            area.x1 - CHEVRON_INSET,
            y + row_h - inset_y,
        );
        let counts = (f.additions > 0 || f.deletions > 0)
            .then(|| format!("+{} -{}", f.additions, f.deletions));
        let counts_w = counts
            .as_deref()
            .map_or(0.0, |s| f64::from(draw::mono_width(tr, s, 11.0)));
        let counts_x = chevron_rect.x0 - COUNTS_GAP - counts_w;
        // Text must end before the counts (or the chevron when there are none).
        let text_right = if counts.is_some() {
            counts_x - COUNTS_GAP
        } else {
            chevron_rect.x0 - COUNTS_GAP
        };
        let name_w = f64::from(
            tr.measure_weighted(f.name(), 12.5, None, TextWeight::Medium)
                .0,
        );
        let name_max_w = (text_right - name_x).max(0.0);
        let dir_x = name_x + name_w.min(name_max_w) + 6.0;
        let dir_max_w = (text_right - dir_x).max(0.0);
        Self {
            badge,
            badge_x,
            name_x,
            name_max_w,
            dir_x,
            dir_max_w,
            counts,
            counts_x,
            chevron: if is_staged { "−" } else { "+" },
            chevron_rect,
        }
    }
}

/// Measured horizontal layout of a section header: the (elided) title, its
/// count badge and the right-anchored bulk action. The action is placed
/// first so the title knows how much of the band it may use; at 2× "Staged
/// Changes 3 Unstage all" used to paint the badge over the action. Title,
/// badge and action are separated by one spacing step (`s2`).
pub(crate) struct HeaderLayout {
    pub title: String,
    pub title_x: f64,
    pub count: String,
    pub badge_x: f64,
    pub badge_h: f64,
    /// Left edge of the bulk action label, when the section has one.
    pub action_x: Option<f64>,
}

impl HeaderLayout {
    /// The header paints a 12 px Semibold title, a 10 px count badge (a 16 px
    /// pill at 1×) and an 11 px Medium bulk action; every measurement here
    /// uses those same sizes.
    pub(crate) fn measure(
        tr: &mut TextRenderer,
        theme: &Theme,
        area: Rect,
        title: &str,
        count: usize,
        action_label: Option<&str>,
    ) -> Self {
        let gap = f64::from(theme.tokens.spacing.s2);
        let action_x = action_label.map(|label| {
            let lw = f64::from(tr.measure_weighted(label, 11.0, None, TextWeight::Medium).0);
            area.x1 - lw - GUTTER
        });
        let count = count.to_string();
        let badge_w = draw::pill_width(tr, &count, 10.0);
        let badge_h = draw::pill_height(tr, 10.0);
        let title_x = area.x0 + GUTTER;
        let title_right = action_x.map_or(area.x1 - GUTTER, |lx| lx - gap);
        let title_max =
            (title_right - gap - badge_w - title_x).max(f64::from(theme.tokens.spacing.s6));
        let title = tr.elide_to_width_weighted(title, 12.0, title_max as f32, TextWeight::Semibold);
        let title_w = f64::from(
            tr.measure_weighted(&title, 12.0, None, TextWeight::Semibold)
                .0,
        );
        Self {
            title,
            title_x,
            count,
            badge_x: title_x + title_w + gap,
            badge_h,
            action_x,
        }
    }
}

/// What the user clicked in the changes column.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChangesHit {
    /// Select a file to view its diff. `(area, index-within-area)`.
    SelectFile(ChangeArea, usize),
    /// Toggle staging on a single file.
    ToggleStage(ChangeArea, usize),
    /// Bulk action header buttons.
    StageAll(ChangeArea),
    UnstageAll(ChangeArea),
    /// Activate the commit button.
    Commit,
    /// Focus the message input.
    FocusMessage,
}

/// One emitted clickable region.
#[derive(Clone, Debug)]
struct HitRegion {
    rect: Rect,
    hit: ChangesHit,
}

/// What a [`ChangesColumn::draw`] pass produced: the click regions plus the
/// rect where the owner should position and draw the commit-message
/// [`ui::widget::TextInput`] (the toolkit input draws its own chrome + caret,
/// so the column reserves the space rather than painting the text itself).
pub struct ChangesDraw {
    pub hits: Vec<Box<dyn Fn(Point) -> Option<ChangesHit>>>,
    /// Exact visible controls/rows for the synthetic accessibility tree.
    pub semantic_hits: Vec<(Rect, ChangesHit)>,
    pub commit_input: Rect,
}

/// Stateless renderer + hit-tester for the changes column.
pub struct ChangesColumn;

impl ChangesColumn {
    /// Draw the column into `area`. `selected` is the currently shown file
    /// (area + index). `message` is the live commit message; `caret` shows
    /// a blinking caret when the box is focused. Returns hit regions for
    /// click routing.
    pub fn draw(
        ctx: &mut PaintCtx<'_>,
        area: Rect,
        model: &RepoModel,
        selected: Option<(ChangeArea, usize)>,
        scroll: f64,
        message: &str,
    ) -> ChangesDraw {
        let c = &ctx.theme.tokens.colors;
        ctx.fill_rect(area, c.surfaces.raised);
        draw::vline(
            ctx,
            area.x1,
            area.y0,
            area.y1,
            draw::edge(c.surfaces.raised),
        );

        let m = Metrics::measure(ctx.text, ctx.theme);
        let mut pass = ColumnPass {
            list_area: Rect::new(area.x0, area.y0, area.x1, area.y1 - m.commit_box_h),
            m,
            selected,
            regions: Vec::new(),
        };

        // Clip the scrollable list.
        ctx.scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            ctx.transform,
            &pass.list_area,
        );

        let mut y = pass.list_area.y0 - scroll;
        y = pass.section(ctx, ChangeArea::Staged, &model.staged, y);
        y = pass.section(ctx, ChangeArea::Unstaged, &model.unstaged, y);
        let _ = pass.section(ctx, ChangeArea::Untracked, &model.untracked, y);

        ctx.scene.pop_layer();

        // Commit box pinned to the bottom. Returns the rect the owner uses to
        // position the commit-message TextInput.
        let commit_input = pass.commit_box(ctx, area, model, message);

        // Build closures that resolve a click to a hit. Returned as a
        // boxed list so the app can route without knowing geometry.
        let regions = pass.regions;
        let semantic_hits = regions
            .iter()
            .map(|region| (region.rect, region.hit.clone()))
            .collect();
        let hits = regions
            .into_iter()
            .map(|r| {
                Box::new(move |p: Point| {
                    if r.rect.contains(p) {
                        Some(r.hit.clone())
                    } else {
                        None
                    }
                }) as Box<dyn Fn(Point) -> Option<ChangesHit>>
            })
            .collect();
        ChangesDraw {
            hits,
            semantic_hits,
            commit_input,
        }
    }
}

/// The heading a section of the column is listed under.
fn section_title(change_area: ChangeArea) -> &'static str {
    match change_area {
        ChangeArea::Staged => "Staged Changes",
        ChangeArea::Unstaged => "Unstaged Changes",
        ChangeArea::Untracked => "Untracked Files",
    }
}

/// One paint pass over the column: the scrollable list viewport, the
/// measured bands, the shown file, and the click regions emitted so far.
struct ColumnPass {
    list_area: Rect,
    m: Metrics,
    selected: Option<(ChangeArea, usize)>,
    regions: Vec<HitRegion>,
}

impl ColumnPass {
    /// Paint the `change_area` section listing `files`, starting at `y`, and
    /// return the `y` the next section starts at.
    fn section(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        change_area: ChangeArea,
        files: &[FileChange],
        y: f64,
    ) -> f64 {
        let m = self.m;
        let area = self.list_area;
        let mut y = self.section_header(ctx, change_area, files, y) + m.row_gap;

        if files.is_empty() {
            let c = &ctx.theme.tokens.colors;
            let hint = "— nothing here —";
            let hint_y =
                draw::centered_top(ctx.text, hint, 12.0, TextWeight::Regular, y, m.empty_hint_h);
            ctx.text(
                hint,
                &TextStyle::new(12.0),
                c.text_dim,
                Point::new(area.x0 + 20.0, hint_y),
                None,
            );
            return y + m.empty_hint_h;
        }

        for (i, f) in files.iter().enumerate() {
            // Rows scrolled out of the list viewport are neither painted nor
            // exposed as hit / semantic regions: the paint layer clips them,
            // and a region under the pinned commit box would otherwise
            // overlap the commit controls.
            if y + m.row_h > area.y0 && y < area.y1 {
                self.section_row(ctx, (change_area, i), f, y);
            }
            y += m.row_h + m.row_gap;
        }
        y + 8.0 - m.row_gap
    }

    /// The section header: title, count badge, and (when non-empty) the
    /// bulk stage/unstage-all action (part of [`Self::section`]). Returns
    /// the `y` the file rows should start at.
    fn section_header(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        change_area: ChangeArea,
        files: &[FileChange],
        y: f64,
    ) -> f64 {
        let theme = ctx.theme;
        let c = &theme.tokens.colors;
        let area = self.list_area;
        let section_h = self.m.section_h;
        let is_staged = change_area == ChangeArea::Staged;
        let action_label = (!files.is_empty()).then_some(if is_staged {
            "Unstage all"
        } else {
            "Stage all"
        });
        let header = HeaderLayout::measure(
            ctx.text,
            theme,
            area,
            section_title(change_area),
            files.len(),
            action_label,
        );
        // Title, count badge and bulk action all centre on the header band:
        // each is placed from its own measured line height rather than a
        // per-element nudge, so they read as one row.
        let title_y = draw::centered_top(
            ctx.text,
            &header.title,
            12.0,
            TextWeight::Semibold,
            y,
            section_h,
        );
        ctx.text(
            &header.title,
            &TextStyle::new(12.0).with_weight(TextWeight::Semibold),
            c.text_dim,
            Point::new(header.title_x, title_y),
            None,
        );
        // Count badge.
        draw::pill(
            ctx,
            Point::new(header.badge_x, y + (section_h - header.badge_h) / 2.0),
            &header.count,
            &draw::PillStyle {
                size: 10.0,
                fg: c.text_primary,
                bg: c.lift(0.72),
                edge: draw::PillEdge::Derived,
            },
        );

        if let Some((label, lx)) = action_label.zip(header.action_x) {
            let label_y =
                draw::centered_top(ctx.text, label, 11.0, TextWeight::Medium, y, section_h);
            ctx.text(
                label,
                &TextStyle::new(11.0).with_weight(TextWeight::Medium),
                theme.accent_primary,
                Point::new(lx, label_y),
                None,
            );
            self.regions.push(HitRegion {
                rect: Rect::new(lx - 4.0, y, area.x1, y + section_h),
                hit: if is_staged {
                    ChangesHit::UnstageAll(change_area)
                } else {
                    ChangesHit::StageAll(change_area)
                },
            });
        }
        y + section_h
    }

    /// One file row at `y`: selection tint, status badge, name/dir, +/-
    /// counts, and the stage/unstage chevron button (part of
    /// [`Self::section`]). `key` is the row's area and index within it.
    fn section_row(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        key: (ChangeArea, usize),
        file: &FileChange,
        y: f64,
    ) {
        let theme = ctx.theme;
        let c = &theme.tokens.colors;
        let area = self.list_area;
        let row_h = self.m.row_h;
        let (change_area, index) = key;
        let is_staged = change_area == ChangeArea::Staged;
        if self.selected == Some(key) {
            paint_row_selection(ctx, Rect::new(area.x0, y, area.x1, y + row_h));
        }

        // Everything in a row sits on one centre line. The +/- counts used to
        // be a second text line at `y + 18` inside the 30px row, which ran
        // into the row below; they now sit right-aligned beside the chevron.
        let layout = RowLayout::measure(ctx.text, theme, area, y, row_h, file, is_staged);

        // Status badge.
        let badge_color = draw::kind_color(theme, file.kind);
        let badge_y = draw::mono_centered_top(ctx.text, layout.badge, 12.0, y, row_h);
        draw::mono(
            ctx,
            layout.badge,
            12.0,
            badge_color,
            Point::new(layout.badge_x, badge_y),
        );

        // Name (bold) + dir (dim). Elide to the allocation: a wrap width
        // would break a long name onto a second line inside the 30px row.
        let name = ctx.text.elide_to_width_weighted(
            file.name(),
            12.5,
            layout.name_max_w as f32,
            TextWeight::Medium,
        );
        let name_y = draw::centered_top(ctx.text, &name, 12.5, TextWeight::Medium, y, row_h);
        ctx.text(
            &name,
            &TextStyle::new(12.5).with_weight(TextWeight::Medium),
            c.text_primary,
            Point::new(layout.name_x, name_y),
            None,
        );
        // A dir hint needs room for more than a lone ellipsis (about three
        // glyphs, one `s6` step); below that it only crowds the counts, so
        // the row shows the name alone.
        let dir = file.dir();
        if !dir.is_empty() && layout.dir_max_w >= f64::from(theme.tokens.spacing.s6) {
            let dir = ctx.text.elide_to_width(dir, 11.0, layout.dir_max_w as f32);
            let dir_y = draw::centered_top(ctx.text, &dir, 11.0, TextWeight::Regular, y, row_h);
            ctx.text(
                &dir,
                &TextStyle::new(11.0),
                c.text_dim,
                Point::new(layout.dir_x, dir_y),
                None,
            );
        }

        // +/- counts, right-aligned against the chevron.
        if let Some(counts) = &layout.counts {
            let counts_y = draw::mono_centered_top(ctx.text, counts, 11.0, y, row_h);
            draw::mono(
                ctx,
                counts,
                11.0,
                c.text_dim,
                Point::new(layout.counts_x, counts_y),
            );
        }

        // Stage / unstage chevron button on the right: a quiet face with its
        // derived rest edge, the +/- glyph centred on its measured line in
        // the semantic add/delete hue.
        let chev = layout.chevron;
        let brect = layout.chevron_rect;
        draw::card(ctx, brect, 4.0, c.lift(0.72));
        let chw = f64::from(draw::mono_width(ctx.text, chev, 14.0));
        let chev_y = draw::mono_centered_top(ctx.text, chev, 14.0, brect.y0, brect.height());
        let chev_color = if is_staged {
            draw::del_fg(theme)
        } else {
            draw::add_fg(theme)
        };
        draw::mono(
            ctx,
            chev,
            14.0,
            chev_color,
            Point::new(brect.x0 + (brect.width() - chw) / 2.0, chev_y),
        );
        // Regions are clamped to the list viewport: a row straddling the
        // bottom edge must not reach under the pinned commit box.
        self.regions.push(HitRegion {
            rect: brect.intersect(area),
            hit: ChangesHit::ToggleStage(change_area, index),
        });

        // Row select region (everything left of the chevron, an `s1` clear of
        // it so the two targets never share pixels).
        self.regions.push(HitRegion {
            rect: Rect::new(
                area.x0,
                y,
                brect.x0 - f64::from(theme.tokens.spacing.s1),
                y + row_h,
            )
            .intersect(area),
            hit: ChangesHit::SelectFile(change_area, index),
        });
    }

    fn commit_box(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        area: Rect,
        model: &RepoModel,
        message: &str,
    ) -> Rect {
        let m = self.m;
        let c = &ctx.theme.tokens.colors;
        let box_rect = Rect::new(area.x0, area.y1 - m.commit_box_h, area.x1, area.y1);
        ctx.fill_rect(box_rect, c.surfaces.base);
        draw::hline(
            ctx,
            box_rect.y0,
            box_rect.x0,
            box_rect.x1,
            draw::edge(c.surfaces.raised),
        );

        let ay = box_rect.y0 + m.commit_pad;
        self.commit_box_author(ctx, box_rect, model, ay);

        // Message input area. The owning app positions + draws a toolkit
        // TextInput here (it paints its own frame, text and caret), and a
        // click in this rect focuses it — so we only reserve the rect and its
        // focus region here. It takes whatever the box has left between the
        // author row and the button row, so the parts never overlap.
        let input_top = ay + m.author_row_h + m.commit_gap;
        let button_top = box_rect.y1 - m.commit_pad - m.button_h;
        let input = Rect::new(
            box_rect.x0 + m.commit_pad,
            input_top,
            box_rect.x1 - m.commit_pad,
            button_top - m.commit_gap,
        );
        self.regions.push(HitRegion {
            rect: input,
            hit: ChangesHit::FocusMessage,
        });

        self.commit_box_button(ctx, box_rect, model, message);

        input
    }

    /// The author identity row at `ay` (avatar + name/email) (part of
    /// [`Self::commit_box`]). The avatar is as tall as the row; the name is
    /// centred on it from its measured line height.
    fn commit_box_author(
        &self,
        ctx: &mut PaintCtx<'_>,
        box_rect: Rect,
        model: &RepoModel,
        ay: f64,
    ) {
        let theme = ctx.theme;
        let c = &theme.tokens.colors;
        let row_h = self.m.author_row_h;
        let pad = self.m.commit_pad;
        let gap = f64::from(theme.tokens.spacing.s2);
        let ax = box_rect.x0 + pad;
        draw::avatar(
            ctx,
            Point::new(ax, ay),
            row_h,
            &model.author_name,
            draw::author_color(theme, &model.author_email),
        );
        let who = if model.author_name.is_empty() {
            "Set git identity".to_string()
        } else {
            format!("{} <{}>", model.author_name, model.author_email)
        };
        let who_max = (box_rect.width() - row_h - 3.0 * pad - gap)
            .max(f64::from(theme.tokens.spacing.s6)) as f32;
        let who = ctx.text.elide_to_width(&who, AUTHOR_FONT, who_max);
        let wy = draw::centered_top(ctx.text, &who, AUTHOR_FONT, TextWeight::Regular, ay, row_h);
        ctx.text(
            &who,
            &TextStyle::new(AUTHOR_FONT),
            c.text_dim,
            Point::new(ax + row_h + gap, wy),
            None,
        );
    }

    /// The commit button and the branch indicator beside it (part of
    /// [`Self::commit_box`]).
    fn commit_box_button(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        box_rect: Rect,
        model: &RepoModel,
        message: &str,
    ) {
        let m = self.m;
        let theme = ctx.theme;
        let c = &theme.tokens.colors;
        let enabled = !model.staged.is_empty() && !message.trim().is_empty();
        let staged_n = model.staged.len();
        let label = if staged_n == 0 {
            "Commit".to_string()
        } else {
            format!(
                "Commit {staged_n} file{}",
                if staged_n == 1 { "" } else { "s" }
            )
        };
        // 12 px Semibold label; the button keeps its 1× width unless the
        // label needs more.
        let (lw, _) = ctx
            .text
            .measure_weighted(&label, 12.0, None, TextWeight::Semibold);
        let pad = m.commit_pad;
        let bw = m.button_w.max(f64::from(lw) + 2.0 * pad);
        let brect = Rect::new(
            box_rect.x1 - pad - bw,
            box_rect.y1 - pad - m.button_h,
            box_rect.x1 - pad,
            box_rect.y1 - pad,
        );
        // The shared face resolves the primary fill and the one opaque
        // disabled look (rest edge included) instead of a hand-mixed pair.
        draw::button(
            ctx,
            &ButtonFace::new(brect, &label)
                .primary()
                .radius(6.0)
                .text_size(12.0)
                .weight(TextWeight::Semibold)
                .disabled(!enabled),
        );
        if enabled {
            self.regions.push(HitRegion {
                rect: brect,
                hit: ChangesHit::Commit,
            });
        }

        // Branch indicator on the left of the button row, on the button's
        // centre line and elided before it.
        let on = format!("on {}", model.head);
        let on_max = (brect.x0 - m.commit_gap - box_rect.x0 - pad)
            .max(f64::from(theme.tokens.spacing.s6)) as f32;
        let on = ctx.text.elide_to_width(&on, AUTHOR_FONT, on_max);
        let oy = draw::centered_top(
            ctx.text,
            &on,
            AUTHOR_FONT,
            TextWeight::Regular,
            brect.y0,
            brect.height(),
        );
        ctx.text(
            &on,
            &TextStyle::new(AUTHOR_FONT),
            c.text_dim,
            Point::new(box_rect.x0 + pad, oy),
            None,
        );
    }
}

/// The selected row's tint: a quiet surface wash keeps selection
/// discoverable without turning a whole row into a saturated accent
/// block. The accent rail remains the strong, stable selection cue.
fn paint_row_selection(ctx: &mut PaintCtx<'_>, row: Rect) {
    let theme = ctx.theme;
    ctx.fill_rect(row, theme.tokens.colors.lift(0.72));
    ctx.fill_rect(
        Rect::new(row.x0, row.y0, row.x0 + 3.0, row.y1),
        theme.accent_primary,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ChangeKind;

    fn change(path: &str, additions: u32, deletions: u32) -> FileChange {
        FileChange {
            path: path.to_string(),
            old_path: None,
            area: ChangeArea::Unstaged,
            kind: ChangeKind::Modified,
            additions,
            deletions,
        }
    }

    /// Changes list layout: rows keep name/dir/counts/chevron on one line (and give
    /// the text the whole row without counts), metrics follow the text scale, rows
    /// keep a token gap on every preset, and header parts never collide. Each phase is a former test with its own fresh fixture.
    #[test]
    fn changes_rows_and_headers_follow_measured_text_without_collisions() {
        row_layout_keeps_name_dir_counts_and_chevron_in_one_line();
        row_layout_without_counts_gives_the_text_the_whole_row();
        metrics_match_the_constants_at_1x_and_grow_with_the_text_scale();
        file_rows_keep_a_token_gap_on_every_preset();
        section_header_title_badge_and_action_never_collide();
    }

    fn row_layout_keeps_name_dir_counts_and_chevron_in_one_line() {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let area = Rect::new(0.0, 0.0, 320.0, 800.0);
        let y = 86.0;
        let file = change("src/render/a-fairly-long-file-name.rs", 124, 6);
        let layout = RowLayout::measure(&mut tr, &theme, area, y, ROW_H, &file, false);
        let row = Rect::new(area.x0, y, area.x1, y + ROW_H);

        assert!(row.contains(layout.chevron_rect.center()));
        assert!(layout.chevron_rect.y0 >= row.y0 && layout.chevron_rect.y1 <= row.y1);
        assert!(layout.chevron_rect.x1 <= area.x1 - CHEVRON_INSET + 0.01);

        let counts = layout.counts.as_deref().expect("changed file has counts");
        let counts_w = f64::from(draw::mono_width(&mut tr, counts, 11.0));
        assert!(
            layout.counts_x + counts_w <= layout.chevron_rect.x0 - COUNTS_GAP + 0.01,
            "counts end at {} but the chevron starts at {}",
            layout.counts_x + counts_w,
            layout.chevron_rect.x0
        );
        assert!(
            layout.dir_x + layout.dir_max_w <= layout.counts_x - COUNTS_GAP + 0.01,
            "dir hint may not run into the counts"
        );
        assert!(layout.name_x + layout.name_max_w <= layout.counts_x - COUNTS_GAP + 0.01);
        assert!(layout.dir_x > layout.name_x);
    }

    fn row_layout_without_counts_gives_the_text_the_whole_row() {
        let mut tr = TextRenderer::new();
        let area = Rect::new(0.0, 0.0, 320.0, 800.0);
        let file = FileChange {
            path: "docs/rendering.md".to_string(),
            old_path: None,
            area: ChangeArea::Untracked,
            kind: ChangeKind::Untracked,
            additions: 0,
            deletions: 0,
        };
        let layout = RowLayout::measure(&mut tr, &Theme::dark(), area, 0.0, ROW_H, &file, false);
        assert!(layout.counts.is_none());
        assert!(
            (layout.name_x + layout.name_max_w - (layout.chevron_rect.x0 - COUNTS_GAP)).abs()
                < 0.01,
            "text runs up to the chevron gap when there are no counts"
        );
    }

    /// The measured bands reproduce the 1× constants exactly — a user who has
    /// not enlarged text sees the same column — and grow with the glyphs at 2×
    /// so 25 px names no longer sit in 30 px rows with no leading.
    fn metrics_match_the_constants_at_1x_and_grow_with_the_text_scale() {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        assert_eq!(Metrics::measure(&mut tr, &theme), Metrics::default());

        tr.set_text_scale(2.0);
        let two = Metrics::measure(&mut tr, &theme);
        let (_, name_h) = tr.measure_weighted("Ag", NAME_FONT, None, TextWeight::Medium);
        assert!(
            two.row_h >= f64::from(name_h) + 12.0,
            "2× rows hold the name plus leading"
        );
        assert!(two.section_h > SECTION_H && two.commit_box_h > COMMIT_BOX_H);
        assert!(
            two.commit_box_h
                >= 2.0 * two.commit_pad
                    + two.author_row_h
                    + two.input_h
                    + two.button_h
                    + 2.0 * two.commit_gap
                    - 0.01,
            "the commit box holds its three rows without overlap"
        );
        // The chevron button stays a real pointer target inside the row.
        let file = change("src/a.rs", 1, 1);
        let area = Rect::new(0.0, 0.0, 320.0, 800.0);
        let layout = RowLayout::measure(&mut tr, &theme, area, 0.0, two.row_h, &file, false);
        let inset = f64::from(theme.tokens.spacing.s1);
        assert!(layout.chevron_rect.y1 <= two.row_h - inset + 0.01);
        assert!(layout.chevron_rect.width() > CHEVRON_W);
    }

    /// HIG §7 "nothing touches": neighbouring file rows, and the first row
    /// under its section header, keep an `s1` gap on every preset — the
    /// list pitch is the row plus that gap, never the bare row.
    fn file_rows_keep_a_token_gap_on_every_preset() {
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
            // Two rows laid out at the section pitch never share a pixel row.
            let first = Rect::new(0.0, 0.0, 320.0, m.row_h);
            let second_top = m.row_h + m.row_gap;
            assert!(second_top - first.y1 >= s1, "{}: rows touch", theme.name);
        }
    }

    /// Title, badge and bulk action share a 320 px header band without
    /// touching at 1× and at 2×: the title elides before the badge and the
    /// badge ends before the action starts.
    #[expect(
        clippy::float_cmp,
        reason = "layout reproduces authored whole-pixel sizes and their sums exactly in binary floating point; exact equality is what the test checks"
    )]
    fn section_header_title_badge_and_action_never_collide() {
        let theme = Theme::dark();
        let gap = f64::from(theme.tokens.spacing.s2);
        let area = Rect::new(0.0, 0.0, 320.0, 800.0);
        for scale in [1.0_f32, 2.0] {
            let mut tr = TextRenderer::new();
            tr.set_text_scale(scale);
            let h = HeaderLayout::measure(
                &mut tr,
                &theme,
                area,
                "Unstaged Changes",
                12,
                Some("Unstage all"),
            );
            let action_x = h.action_x.expect("non-empty section has a bulk action");
            // The badge is painted at the header's 10 px label size; its
            // width comes from the same helper the paint uses.
            let badge_end = h.badge_x + draw::pill_width(&mut tr, &h.count, 10.0);
            assert!(
                badge_end <= action_x - gap + 0.01,
                "{scale}×: badge ends at {badge_end} but the action starts at {action_x}"
            );
            let title_w = f64::from(
                tr.measure_weighted(&h.title, 12.0, None, TextWeight::Semibold)
                    .0,
            );
            assert!((h.badge_x - (h.title_x + title_w + gap)).abs() < 0.01);
            if (scale - 1.0).abs() < f32::EPSILON {
                assert_eq!(h.title, "Unstaged Changes", "1× keeps the full title");
                assert_eq!(h.badge_h, 16.0, "1× badge is the historical 16 px pill");
            } else {
                assert!(h.badge_h > 16.0, "2× badge grows with its label");
            }
        }
    }
}
