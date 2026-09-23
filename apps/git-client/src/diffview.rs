//! The diff pane: unified or side-by-side, with syntax-highlighted
//! content and per-line add/remove tinting, line numbers, hunk headers,
//! and a hover affordance for staging individual hunks.
//!
//! Rendering is a single linear pass over the parsed [`FileDiff`] lines,
//! producing one row per line. The layout knows its own row height so the
//! owning view can scroll it and hit-test hunk-stage buttons.

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Color;

use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::ButtonFace;
use ui::widget::code_editor::{SyntaxHighlighter, TokenKind};

use crate::draw;
use crate::model::{DiffLine, DiffLineKind, FileDiff};
use crate::settings::{DiffLayout, Settings};

/// Minimum monospace row height for diff lines, in logical px. The painted
/// pitch is [`Metrics::row_h`]: this value at 1×, the measured line plus
/// leading once the text scale outgrows it.
pub const ROW_H: f64 = 18.0;
const FONT: f32 = 12.5;
/// Minimum unified gutter width (two line-number columns + divider).
const GUTTER_W: f64 = 96.0;

/// Row geometry measured from the renderer the diff paints with.
///
/// Refreshed at the top of every draw and handed back to the owner with the
/// hunk buttons, so scrolling, hit rects and paint share one pitch: a text
/// scale that grows the glyphs grows the rows and the gutter with them
/// instead of letting 30 px glyphs overprint 18 px rows. `Default` is the
/// 1× geometry the view has always painted (columns right-aligned at 40 and
/// 84, a 4 px inset around each number and the divider); `measure` returns
/// exactly that until the line height or the widest line number outgrows it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Metrics {
    /// Row pitch.
    pub row_h: f64,
    /// Offset from a row's top that centres a `FONT`-sized monospace line.
    text_dy: f64,
    /// Right edge of the old-line-number column, relative to the pane.
    old_col_right: f64,
    /// Right edge of the new-line-number column, relative to the pane.
    new_col_right: f64,
    /// Unified gutter width (both columns, divider and its inset).
    gutter_w: f64,
    /// Air kept before each line number and around the gutter divider.
    inset: f64,
}

impl Default for Metrics {
    fn default() -> Self {
        Self {
            row_h: ROW_H,
            text_dy: (ROW_H - 15.625) / 2.0,
            old_col_right: 40.0,
            new_col_right: 84.0,
            gutter_w: GUTTER_W,
            inset: 4.0,
        }
    }
}

impl Metrics {
    /// Measure the pitch and gutter for `diff` with the exact mono style the
    /// rows are painted in. At 1× every value equals `Default`; the columns
    /// widen only when the widest line number no longer fits.
    pub fn measure(tr: &mut TextRenderer, diff: &FileDiff, theme: &Theme) -> Self {
        let one = Self::default();
        let (_, line_h) = tr.measure_styled("M", FONT, None, TextWeight::Regular, false, true);
        let line_h = f64::from(line_h);
        // 1 px of leading either side: a 12.5 px mono line measures 15.625 px,
        // so 18 px rows keep ~1.2 px and only a larger text scale grows them.
        let row_h = (line_h + 2.0).max(ROW_H);
        // Line numbers are monotonic within a diff, so the last numbered line
        // of each column carries its maximum — two O(1) reverse scans rather
        // than a walk over every line on every frame.
        let last_old = diff.lines.iter().rev().find_map(|l| l.old_lineno);
        let last_new = diff.lines.iter().rev().find_map(|l| l.new_lineno);
        let digits = last_old
            .max(last_new)
            .map_or(2, |n| n.to_string().len().max(2));
        let widest = "0".repeat(digits);
        let num_w = f64::from(
            tr.measure_styled(&widest, FONT, None, TextWeight::Regular, false, true)
                .0,
        );
        let inset = f64::from(theme.tokens.spacing.s1);
        let old_col_right = (num_w + 2.0 * inset).max(one.old_col_right);
        let col_pitch = (num_w + 2.0 * inset).max(one.new_col_right - one.old_col_right);
        let new_col_right = old_col_right + col_pitch;
        let gutter_w = (new_col_right + 3.0 * inset).max(GUTTER_W);
        Self {
            row_h,
            text_dy: (row_h - line_h) / 2.0,
            old_col_right,
            new_col_right,
            gutter_w,
            inset,
        }
    }
}

/// What one [`DiffView::draw`] pass produced: the hunk-action hit rects and
/// the row metrics the owner needs to clamp scrolling to the same pitch.
pub struct DiffDraw {
    pub buttons: Vec<HunkButton>,
    pub metrics: Metrics,
}

/// What a hunk-action button does when clicked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HunkAction {
    /// Stage this hunk (unstaged side) or unstage it (staged side).
    Toggle,
    /// Discard this hunk's worktree change (unstaged side only).
    Discard,
}

/// A clickable hunk-action button rect emitted during draw, so the owning
/// view can map clicks back to a hunk index + action.
#[derive(Clone, Copy, Debug)]
pub struct HunkButton {
    pub hunk: usize,
    pub action: HunkAction,
    pub rect: Rect,
}

/// Per-frame hunk action state supplied by the owning app.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HunkInteraction {
    pub hovered: Option<usize>,
    pub staged_side: bool,
    pub armed_discard: Option<usize>,
}

/// Stateless renderer for a file diff. The owning view supplies the
/// viewport rect, scroll offset and settings; hit-test data comes back via
/// the returned [`HunkButton`]s.
pub struct DiffView;

impl DiffView {
    /// Total content height using the stable 1x row pitch.
    pub fn content_height(diff: &FileDiff) -> f64 {
        diff.lines.len() as f64 * ROW_H + 8.0
    }

    /// Total content height for `diff` at the row pitch of the last draw
    /// (used to clamp scrolling).
    pub fn content_height_with_metrics(diff: &FileDiff, metrics: &Metrics) -> f64 {
        diff.lines.len() as f64 * metrics.row_h + 8.0
    }

    /// Draw through the original paint-context API.
    pub fn draw(
        paint: &mut draw::PaintCtx<'_>,
        theme: &Theme,
        viewport: Rect,
        diff: &FileDiff,
        scroll: f64,
        settings: &Settings,
        interaction: HunkInteraction,
    ) -> Vec<HunkButton> {
        let (scene, text, transform) = paint.parts();
        Self::draw_with_metrics(
            scene,
            text,
            transform,
            theme,
            viewport,
            diff,
            scroll,
            settings,
            interaction.hovered,
            interaction.staged_side,
            interaction.armed_discard,
        )
        .buttons
    }

    /// Draw the diff into `viewport`, scrolled down by `scroll` px.
    /// `hover_hunk` highlights the stage button on that hunk. Returns the
    /// hunk-stage button rects for hit-testing and the row metrics painted.
    #[allow(
        clippy::too_many_arguments,
        reason = "diff painting combines paint context, viewport and scroll with settings and the hovered, staged and armed hunk states"
    )]
    pub fn draw_with_metrics(
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        viewport: Rect,
        diff: &FileDiff,
        scroll: f64,
        settings: &Settings,
        hover_hunk: Option<usize>,
        staged_side: bool,
        armed_hunk: Option<usize>,
    ) -> DiffDraw {
        let metrics = Metrics::measure(tr, diff, theme);
        let buttons = Self::draw_rows(
            scene,
            tr,
            t,
            theme,
            viewport,
            diff,
            scroll,
            settings,
            hover_hunk,
            staged_side,
            armed_hunk,
            &metrics,
        );
        DiffDraw { buttons, metrics }
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "row painting receives the diff viewport, settings, measured metrics and hunk interaction states alongside paint context"
    )]
    fn draw_rows(
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        viewport: Rect,
        diff: &FileDiff,
        scroll: f64,
        settings: &Settings,
        hover_hunk: Option<usize>,
        staged_side: bool,
        armed_hunk: Option<usize>,
        m: &Metrics,
    ) -> Vec<HunkButton> {
        let c = &theme.tokens.colors;
        // Pane background.
        draw::fill_rect(scene, t, viewport, c.surfaces.base);

        if diff.binary {
            draw::text(
                scene,
                tr,
                t,
                "Binary file — no textual diff",
                14.0,
                c.text_dim,
                viewport.x0 + 24.0,
                viewport.y0 + 24.0,
                None,
            );
            return Vec::new();
        }
        if diff.lines.is_empty() {
            draw::text(
                scene,
                tr,
                t,
                "No changes to display",
                14.0,
                c.text_dim,
                viewport.x0 + 24.0,
                viewport.y0 + 24.0,
                None,
            );
            return Vec::new();
        }

        // Clip to the viewport so scrolled rows don't bleed.
        scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            t,
            &viewport,
        );

        let result = if settings.diff_layout == DiffLayout::SideBySide {
            Self::draw_split(scene, tr, t, theme, viewport, diff, scroll, *settings, m)
        } else {
            Self::draw_unified(
                scene,
                tr,
                t,
                theme,
                viewport,
                diff,
                scroll,
                *settings,
                hover_hunk,
                staged_side,
                armed_hunk,
                m,
            )
        };

        scene.pop_layer();
        result
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "unified rows use paint context, scrolling metrics, diff data and hunk interaction states to place code and action buttons"
    )]
    fn draw_unified(
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        viewport: Rect,
        diff: &FileDiff,
        scroll: f64,
        settings: Settings,
        hover_hunk: Option<usize>,
        staged_side: bool,
        armed_hunk: Option<usize>,
        m: &Metrics,
    ) -> Vec<HunkButton> {
        let mut buttons: Vec<HunkButton> = Vec::new();
        let x0 = viewport.x0;
        let top = viewport.y0 - scroll;
        let mut last_hunk: Option<usize> = None;

        for (i, line) in diff.lines.iter().enumerate() {
            let y = top + i as f64 * m.row_h;
            if y + m.row_h < viewport.y0 || y > viewport.y1 {
                continue; // cull off-screen rows
            }
            let row = Rect::new(x0, y, viewport.x1, y + m.row_h);
            let text_limit = Self::draw_unified_row_background(
                scene,
                tr,
                t,
                theme,
                row,
                line,
                hover_hunk,
                staged_side,
                armed_hunk,
                &mut last_hunk,
                &mut buttons,
            );
            Self::draw_unified_row_content(
                scene, tr, t, theme, diff, line, x0, y, settings, m, text_limit,
            );
        }
        buttons
    }

    /// The row's tinted background, and — on a hunk's first row — its
    /// stage/unstage (+ worktree-side discard) buttons (part of
    /// [`Self::draw_unified`]). Returns the x the row's text must end before
    /// when buttons occupy its right end, so a long `@@ … @@` header elides
    /// instead of running under `Unstage hunk`.
    #[allow(clippy::too_many_arguments, reason = "see the comment above txt")]
    fn draw_unified_row_background(
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        row: Rect,
        line: &DiffLine,
        hover_hunk: Option<usize>,
        staged_side: bool,
        armed_hunk: Option<usize>,
        last_hunk: &mut Option<usize>,
        buttons: &mut Vec<HunkButton>,
    ) -> Option<f64> {
        let c = &theme.tokens.colors;
        match line.kind {
            DiffLineKind::Addition => draw::fill_rect(scene, t, row, draw::add_tint(theme)),
            DiffLineKind::Deletion => draw::fill_rect(scene, t, row, draw::del_tint(theme)),
            DiffLineKind::Hunk => {
                Self::hunk_band(scene, t, theme, row);
                // Emit the hunk action button(s) on the first row of the
                // hunk: always a stage/unstage toggle, plus a discard button
                // on the worktree (unstaged) side.
                if Some(line.hunk) != *last_hunk {
                    let hovered = hover_hunk == Some(line.hunk);
                    let toggle_label = if staged_side {
                        "Unstage hunk"
                    } else {
                        "Stage hunk"
                    };
                    // Buttons sit one spacing step from the pane edge and
                    // from each other; the header text ends the same step
                    // before the leftmost button.
                    let gap = f64::from(theme.tokens.spacing.s2);
                    let mut right = row.x1 - gap;
                    let toggle = Self::draw_hunk_button(
                        scene,
                        tr,
                        t,
                        theme,
                        &row,
                        right,
                        line.hunk,
                        HunkAction::Toggle,
                        toggle_label,
                        hovered,
                    );
                    right = toggle.rect.x0 - gap;
                    let mut leftmost = toggle.rect.x0;
                    buttons.push(toggle);
                    if !staged_side {
                        let armed = armed_hunk == Some(line.hunk);
                        let dlabel = if armed { "Discard?" } else { "Discard" };
                        let discard = Self::draw_hunk_button(
                            scene,
                            tr,
                            t,
                            theme,
                            &row,
                            right,
                            line.hunk,
                            HunkAction::Discard,
                            dlabel,
                            hovered || armed,
                        );
                        leftmost = discard.rect.x0;
                        buttons.push(discard);
                    }
                    *last_hunk = Some(line.hunk);
                    return Some(leftmost - gap);
                }
            }
            DiffLineKind::Meta => draw::fill_rect(scene, t, row, c.surfaces.raised),
            DiffLineKind::Context => {}
        }
        None
    }

    /// The line-number gutter, marker glyph, and syntax-highlighted (or
    /// plain) content for one row (part of [`Self::draw_unified`]).
    /// `text_limit` is the x a hunk header must end before (the first hunk
    /// button); code rows are clipped by the pane instead.
    #[allow(clippy::too_many_arguments, reason = "see the comment above txt")]
    fn draw_unified_row_content(
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        diff: &FileDiff,
        line: &DiffLine,
        x0: f64,
        y: f64,
        settings: Settings,
        m: &Metrics,
        text_limit: Option<f64>,
    ) {
        let c = &theme.tokens.colors;
        let ty = y + m.text_dy;
        Self::draw_linenos(scene, tr, t, theme, x0, y, line, m);

        // Marker glyph + content.
        let content_x = x0 + m.gutter_w;
        let marker = match line.kind {
            DiffLineKind::Addition => "+",
            DiffLineKind::Deletion => "-",
            _ => " ",
        };
        let marker_color = match line.kind {
            DiffLineKind::Addition => draw::add_fg(theme),
            DiffLineKind::Deletion => draw::del_fg(theme),
            _ => c.text_dim,
        };
        draw::mono(scene, tr, t, marker, FONT, marker_color, content_x, ty);

        let text_x = content_x + 14.0;
        match line.kind {
            DiffLineKind::Hunk => {
                let header = match text_limit {
                    Some(limit) => draw::elide_mono(tr, &line.content, FONT, limit - content_x),
                    None => line.content.clone(),
                };
                draw::mono(scene, tr, t, &header, FONT, c.semantic.info, content_x, ty);
            }
            DiffLineKind::Meta => {
                draw::mono(scene, tr, t, &line.content, FONT, c.text_dim, content_x, ty);
            }
            _ => Self::draw_code(
                scene,
                tr,
                t,
                theme,
                &line.content,
                &diff.language,
                text_x,
                ty,
                settings.syntax_highlight,
            ),
        }
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "split rows use paint context, viewport, scroll and measured metrics to align old/new lines under the selected settings"
    )]
    fn draw_split(
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        viewport: Rect,
        diff: &FileDiff,
        scroll: f64,
        settings: Settings,
        m: &Metrics,
    ) -> Vec<HunkButton> {
        let c = &theme.tokens.colors;
        let mid = f64::midpoint(viewport.x0, viewport.x1);
        draw::vline(scene, t, mid, viewport.y0, viewport.y1, c.border_standard);

        let top = viewport.y0 - scroll;
        // Pair deletions/additions into aligned rows. We walk lines and
        // emit left (old) / right (new) cells; context spans both.
        let rows = pair_rows(&diff.lines);
        for (i, pr) in rows.iter().enumerate() {
            let y = top + i as f64 * m.row_h;
            if y + m.row_h < viewport.y0 || y > viewport.y1 {
                continue;
            }
            let left = Rect::new(viewport.x0, y, mid, y + m.row_h);
            let right = Rect::new(mid, y, viewport.x1, y + m.row_h);

            // `pr.left`/`pr.right` are always valid indices into this same
            // `diff.lines` (`pair_rows` builds them from it), but `.get`
            // degrades to skipping the half-row instead of indexing blindly
            // into text parsed from a repository someone else wrote.
            if let Some(li) = pr.left
                && let Some(line) = diff.lines.get(li)
            {
                if line.kind == DiffLineKind::Deletion {
                    draw::fill_rect(scene, t, left, draw::del_tint(theme));
                } else if line.kind == DiffLineKind::Hunk {
                    Self::hunk_band(scene, t, theme, left);
                } else if line.kind == DiffLineKind::Meta {
                    draw::fill_rect(scene, t, left, c.surfaces.raised);
                }
                Self::draw_half(scene, tr, t, theme, line, viewport.x0, y, settings, true, m);
            }
            if let Some(ri) = pr.right
                && let Some(line) = diff.lines.get(ri)
            {
                if line.kind == DiffLineKind::Addition {
                    draw::fill_rect(scene, t, right, draw::add_tint(theme));
                } else if line.kind == DiffLineKind::Hunk {
                    Self::hunk_band(scene, t, theme, right);
                } else if line.kind == DiffLineKind::Meta {
                    draw::fill_rect(scene, t, right, c.surfaces.raised);
                }
                Self::draw_half(scene, tr, t, theme, line, mid, y, settings, false, m);
            }
        }
        Vec::new()
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "half-row painting combines paint context, line data and origin with display settings, side and measured gutter metrics"
    )]
    #[allow(
        clippy::many_single_char_names,
        reason = "t/y/c follow this file's graphics convention (transform, position, colour tokens); n/s/w are the line number and its formatted string/measured width"
    )]
    fn draw_half(
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        line: &DiffLine,
        x0: f64,
        y: f64,
        settings: Settings,
        is_left: bool,
        m: &Metrics,
    ) {
        let c = &theme.tokens.colors;
        let ty = y + m.text_dy;
        let no = if is_left {
            line.old_lineno
        } else {
            line.new_lineno
        };
        // One number column per half: as wide as the unified old column plus
        // its inset (44 px at 1×), growing with the widest line number.
        let col_right = x0 + m.old_col_right + m.inset;
        if let Some(n) = no {
            let s = n.to_string();
            let w = f64::from(draw::mono_width(tr, &s, FONT));
            draw::mono(scene, tr, t, &s, FONT, c.text_dim, col_right - w, ty);
        }
        let text_x = col_right + 10.0;
        match line.kind {
            DiffLineKind::Hunk => {
                draw::mono(
                    scene,
                    tr,
                    t,
                    &line.content,
                    FONT,
                    c.semantic.info,
                    text_x,
                    ty,
                );
            }
            DiffLineKind::Meta => {
                draw::mono(scene, tr, t, &line.content, FONT, c.text_dim, text_x, ty);
            }
            _ => Self::draw_code(
                scene,
                tr,
                t,
                theme,
                &line.content,
                "text",
                text_x,
                ty,
                settings.syntax_highlight,
            ),
        }
    }

    fn draw_linenos(
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        x0: f64,
        y: f64,
        line: &DiffLine,
        m: &Metrics,
    ) {
        let c = &theme.tokens.colors;
        let ty = y + m.text_dy;
        let old = line.old_lineno.map(|n| n.to_string()).unwrap_or_default();
        let new = line.new_lineno.map(|n| n.to_string()).unwrap_or_default();
        // Right-align both columns within the gutter.
        let ow = f64::from(draw::mono_width(tr, &old, FONT));
        let nw = f64::from(draw::mono_width(tr, &new, FONT));
        draw::mono(
            scene,
            tr,
            t,
            &old,
            FONT,
            c.text_dim,
            x0 + m.old_col_right - ow,
            ty,
        );
        draw::mono(
            scene,
            tr,
            t,
            &new,
            FONT,
            c.text_dim,
            x0 + m.new_col_right - nw,
            ty,
        );
        draw::vline(
            scene,
            t,
            x0 + m.gutter_w - m.inset,
            y,
            y + m.row_h,
            c.border_standard,
        );
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "code painting keeps the renderer inputs, text origin, language and highlighting toggle explicit"
    )]
    fn draw_code(
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        content: &str,
        language: &str,
        x: f64,
        y: f64,
        highlight: bool,
    ) {
        let c = &theme.tokens.colors;
        if !highlight {
            draw::mono(scene, tr, t, content, FONT, c.text_primary, x, y);
            return;
        }
        // Diff rows are discontiguous slices of the file, so each line is
        // tokenized on its own.
        let mut cx = x;
        let spans = SyntaxHighlighter::for_token_or_plain(language)
            .start()
            .highlight_line(content);
        for span in spans {
            let Some(text) = content.get(span.range) else {
                continue;
            };
            let color = token_color(theme, span.kind);
            draw::mono(scene, tr, t, text, FONT, color, cx, y);
            cx += f64::from(draw::mono_width(tr, text, FONT));
        }
    }

    /// A hunk header row: the raised band closed top and bottom by its
    /// derived hairline, so the band reads as a surface on the dark preset
    /// instead of a raised fill blurring into the raised gutter (HIG §9.5).
    fn hunk_band(scene: &mut Scene, t: Affine, theme: &Theme, row: Rect) {
        let raised = theme.tokens.colors.surfaces.raised;
        draw::fill_rect(scene, t, row, raised);
        let edge = draw::edge(raised);
        draw::hline(scene, t, row.y0 + 0.5, row.x0, row.x1, edge);
        draw::hline(scene, t, row.y1 - 0.5, row.x0, row.x1, edge);
    }

    /// Draw one hunk-action button whose right edge sits at `right_x`, and
    /// return its hit rect. Discard buttons read in the danger hue when hovered
    /// so the destructive action is visually distinct from stage/unstage.
    #[allow(
        clippy::too_many_arguments,
        reason = "hunk buttons combine paint context and alignment with the hunk/action identity, label and hover state"
    )]
    fn draw_hunk_button(
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        row: &Rect,
        right_x: f64,
        hunk: usize,
        action: HunkAction,
        label: &str,
        hovered: bool,
    ) -> HunkButton {
        // 11 px Medium label with 10 px of padding either side, inset 2 px
        // from the row's top and bottom (the button is as tall as its row).
        let size = 11.0f32;
        let (tw, _) = tr.measure_weighted(label, size, None, TextWeight::Medium);
        let tw = f64::from(tw);
        let bw = tw + 20.0;
        let bx = right_x - bw;
        let by = row.y0 + 2.0;
        let brect = Rect::new(bx, by, bx + bw, row.y1 - 2.0);
        // Quiet at rest (neutral face with its derived edge, or the danger
        // outline for discard), filled in the action's hue under the pointer.
        let face = ButtonFace::new(brect, label).radius(5.0).text_size(size);
        let face = match (action, hovered) {
            (HunkAction::Toggle, true) => face.primary(),
            (HunkAction::Toggle, false) => face,
            (HunkAction::Discard, true) => face.danger(),
            (HunkAction::Discard, false) => face.danger_outline(),
        };
        draw::button(scene, tr, t, theme, &face);
        HunkButton {
            hunk,
            action,
            rect: brect,
        }
    }
}

fn token_color(theme: &Theme, kind: TokenKind) -> Color {
    let c = &theme.tokens.colors;
    match kind {
        TokenKind::Keyword => c.accent[6],
        TokenKind::StringLit | TokenKind::CharLit => c.semantic.success,
        TokenKind::Comment => c.text_dim,
        TokenKind::Number | TokenKind::Constant => c.semantic.warning,
        TokenKind::Type | TokenKind::Function => c.semantic.info,
        TokenKind::Operator | TokenKind::Punctuation => c.neutral[7],
        TokenKind::Attribute | TokenKind::Preprocessor => c.semantic.danger,
        _ => c.text_primary,
    }
}

/// A side-by-side aligned row: an optional left (old) line index and an
/// optional right (new) line index.
struct PairRow {
    left: Option<usize>,
    right: Option<usize>,
}

/// Pair deletions and additions for side-by-side display. Context, hunk
/// and meta lines occupy both columns; a run of deletions aligns against
/// the following run of additions so changed pairs sit on the same row.
fn pair_rows(lines: &[DiffLine]) -> Vec<PairRow> {
    let mut rows: Vec<PairRow> = Vec::new();
    let mut i = 0;
    // `.get`/`is_some_and` rather than `i < lines.len()` + `lines[i]` — this
    // walks text parsed from a repository someone else wrote, so the bound
    // is re-checked at every step instead of trusted across statements.
    while let Some(cur) = lines.get(i) {
        match cur.kind {
            DiffLineKind::Context | DiffLineKind::Hunk | DiffLineKind::Meta => {
                rows.push(PairRow {
                    left: Some(i),
                    right: Some(i),
                });
                i += 1;
            }
            DiffLineKind::Deletion => {
                // Gather the run of deletions, then the run of additions.
                let del_start = i;
                while lines
                    .get(i)
                    .is_some_and(|l| l.kind == DiffLineKind::Deletion)
                {
                    i += 1;
                }
                let del_end = i;
                let add_start = i;
                while lines
                    .get(i)
                    .is_some_and(|l| l.kind == DiffLineKind::Addition)
                {
                    i += 1;
                }
                let add_end = i;
                let dels: Vec<usize> = (del_start..del_end).collect();
                let adds: Vec<usize> = (add_start..add_end).collect();
                let n = dels.len().max(adds.len());
                for k in 0..n {
                    rows.push(PairRow {
                        left: dels.get(k).copied(),
                        right: adds.get(k).copied(),
                    });
                }
            }
            DiffLineKind::Addition => {
                // Stray addition with no preceding deletion.
                rows.push(PairRow {
                    left: None,
                    right: Some(i),
                });
                i += 1;
            }
        }
    }
    rows
}

/// Hit-test `pos` against the emitted hunk buttons, returning the hunk index
/// (for hover highlighting — any button of a hunk highlights the hunk).
pub fn hunk_button_at(buttons: &[HunkButton], pos: Point) -> Option<usize> {
    buttons
        .iter()
        .find(|b| b.rect.contains(pos))
        .map(|b| b.hunk)
}

/// Hit-test `pos` against the emitted hunk buttons, returning the hunk index
/// and which action its button performs.
pub fn hunk_action_at(buttons: &[HunkButton], pos: Point) -> Option<(usize, HunkAction)> {
    buttons
        .iter()
        .find(|b| b.rect.contains(pos))
        .map(|b| (b.hunk, b.action))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::parse_unified;

    #[test]
    fn pair_rows_aligns_change_blocks() {
        let d = parse_unified(
            "x.rs",
            "rust",
            "@@ -1,2 +1,2 @@\n ctx\n-old line\n+new line\n",
        );
        let rows = pair_rows(&d.lines);
        // Find the row pairing the deletion with the addition.
        let paired = rows.iter().find(|r| {
            r.left.is_some()
                && r.right.is_some()
                && d.lines[r.left.unwrap()].kind == DiffLineKind::Deletion
        });
        assert!(
            paired.is_some(),
            "deletion should pair against the addition"
        );
    }

    #[test]
    fn content_height_scales_with_lines() {
        let d = parse_unified("x.rs", "rust", "@@ -1,1 +1,1 @@\n+a\n+b\n");
        assert!(DiffView::content_height_with_metrics(&d, &Metrics::default()) > ROW_H * 2.0);
    }

    /// The measured metrics reproduce the historical constants at 1× — the
    /// scaled path changes nothing for a user who has not enlarged text — and
    /// grow the pitch and gutter with the glyphs at 2×.
    #[test]
    fn metrics_match_the_constants_at_1x_and_follow_the_text_scale() {
        let d = parse_unified("x.rs", "rust", "@@ -10,3 +10,4 @@\n a\n-b\n+c\n+d\n");
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let one = Metrics::measure(&mut tr, &d, &theme);
        assert_eq!(
            one,
            Metrics::default(),
            "1× measures exactly the historical geometry"
        );
        assert_eq!(one.row_h, ROW_H);
        assert_eq!(one.gutter_w, GUTTER_W);

        tr.set_text_scale(2.0);
        let (_, line_h) = tr.measure_styled("M", FONT, None, TextWeight::Regular, false, true);
        let two = Metrics::measure(&mut tr, &d, &theme);
        assert!(
            two.row_h >= f64::from(line_h) + 2.0,
            "2× rows ({}) must hold a {line_h}px line plus leading",
            two.row_h
        );
        assert!(two.row_h > one.row_h && two.gutter_w >= one.gutter_w);
        // The content height the owner clamps scrolling to follows the pitch.
        assert!(
            DiffView::content_height_with_metrics(&d, &two)
                > DiffView::content_height_with_metrics(&d, &one)
        );
    }

    /// The gutter takes its digit count from the last numbered line of each
    /// column, so a deletion tail with a larger old number than the final
    /// addition's new number is not missed.
    #[test]
    fn gutter_digits_come_from_the_widest_column_tail() {
        let d = parse_unified("x.rs", "rust", "@@ -998,3 +1,2 @@\n a\n-b\n-c\n+d\n");
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        tr.set_text_scale(2.0);
        let wide = Metrics::measure(&mut tr, &d, &theme);
        let narrow = Metrics::measure(
            &mut tr,
            &parse_unified("x.rs", "rust", "@@ -1,3 +1,2 @@\n a\n-b\n-c\n+d\n"),
            &theme,
        );
        assert!(
            wide.gutter_w > narrow.gutter_w,
            "four-digit old numbers widen the gutter"
        );
    }

    /// A long `@@ … @@` header must end before the hunk's first action button
    /// in a phone-width pane instead of running under it.
    #[test]
    fn hunk_header_elides_before_the_unstage_button() {
        let d = parse_unified(
            "x.rs",
            "rust",
            "@@ -12,9 +12,11 @@ impl Scene for a very long context that overflows\n a\n-b\n+c\n",
        );
        let mut tr = TextRenderer::new();
        let mut scene = Scene::new();
        let theme = Theme::dark();
        let viewport = Rect::new(0.0, 0.0, 390.0, 600.0);
        let settings = Settings::default();
        let out = DiffView::draw_with_metrics(
            &mut scene,
            &mut tr,
            Affine::IDENTITY,
            &theme,
            viewport,
            &d,
            0.0,
            &settings,
            None,
            true,
            None,
        );
        let toggle = out
            .buttons
            .iter()
            .find(|b| b.action == HunkAction::Toggle)
            .expect("staged side emits an unstage button");
        let m = out.metrics;
        let content_x = viewport.x0 + m.gutter_w;
        let limit = toggle.rect.x0 - f64::from(theme.tokens.spacing.s2);
        let header = &d.lines[0].content;
        let elided = draw::elide_mono(&mut tr, header, FONT, limit - content_x);
        assert!(
            elided.ends_with('\u{2026}'),
            "header {header:?} should elide at 390px"
        );
        let painted_w = f64::from(draw::mono_width(&mut tr, &elided, FONT));
        assert!(
            content_x + painted_w <= limit + 0.01,
            "header ends at {} but the button starts at {}",
            content_x + painted_w,
            toggle.rect.x0
        );
        // The button sits inside its row at the row's pitch (2 px inset).
        assert!((toggle.rect.height() - (m.row_h - 4.0)).abs() < 0.01);
    }
}
