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

use crate::diff::{TokenClass, tokenize};
use crate::draw;
use crate::model::{DiffLine, DiffLineKind, FileDiff};
use crate::settings::{DiffLayout, Settings};

/// Fixed monospace row height for diff lines, in logical px.
pub const ROW_H: f64 = 18.0;
const FONT: f32 = 12.5;
const GUTTER_W: f64 = 96.0;

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

/// Resources and stable inputs shared by one diff paint pass.
struct DiffPaintCtx<'a> {
    scene: &'a mut Scene,
    text: &'a mut TextRenderer,
    transform: Affine,
    theme: &'a Theme,
    viewport: Rect,
    diff: &'a FileDiff,
    settings: Settings,
    interaction: HunkInteraction,
}

struct HunkButtonSpec<'a> {
    row: Rect,
    right_x: f64,
    hunk: usize,
    action: HunkAction,
    label: &'a str,
    hovered: bool,
}

/// Stateless renderer for a file diff. The owning view supplies the
/// viewport rect, scroll offset and settings; hit-test data comes back via
/// the returned [`HunkButton`]s.
pub struct DiffView;

impl DiffView {
    /// Total content height for `diff` (used to clamp scrolling).
    pub fn content_height(diff: &FileDiff) -> f64 {
        diff.lines.len() as f64 * ROW_H + 8.0
    }

    /// Draw the diff into `viewport`, scrolled down by `scroll` px.
    /// `hover_hunk` highlights the stage button on that hunk. Returns the
    /// hunk-stage button rects for hit-testing.
    pub fn draw(
        paint: &mut draw::PaintCtx<'_>,
        theme: &Theme,
        viewport: Rect,
        diff: &FileDiff,
        scroll: f64,
        settings: &Settings,
        interaction: HunkInteraction,
    ) -> Vec<HunkButton> {
        let (scene, tr, t) = paint.parts();
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

        let mut ctx = DiffPaintCtx {
            scene,
            text: tr,
            transform: t,
            theme,
            viewport,
            diff,
            settings: *settings,
            interaction,
        };

        let result = if settings.diff_layout == DiffLayout::SideBySide {
            Self::draw_split(&mut ctx, scroll)
        } else {
            Self::draw_unified(&mut ctx, scroll)
        };

        ctx.scene.pop_layer();
        result
    }

    fn draw_unified(ctx: &mut DiffPaintCtx<'_>, scroll: f64) -> Vec<HunkButton> {
        let mut buttons: Vec<HunkButton> = Vec::new();
        let x0 = ctx.viewport.x0;
        let top = ctx.viewport.y0 - scroll;
        let mut last_hunk: Option<usize> = None;

        for (i, line) in ctx.diff.lines.iter().enumerate() {
            let y = top + i as f64 * ROW_H;
            if y + ROW_H < ctx.viewport.y0 || y > ctx.viewport.y1 {
                continue; // cull off-screen rows
            }
            let row = Rect::new(x0, y, ctx.viewport.x1, y + ROW_H);
            let previous_buttons = buttons.len();
            Self::draw_unified_row_background(ctx, row, line, &mut last_hunk, &mut buttons);
            // Header text must not paint over the action buttons. Use their
            // actual measured bounds, including the wider armed-discard label.
            let text_right = buttons
                .get(previous_buttons..)
                .unwrap_or_default()
                .iter()
                .fold(row.x1, |right, button| right.min(button.rect.x0 - 8.0));
            if line.kind == DiffLineKind::Hunk {
                ctx.scene.push_layer(
                    vello::peniko::Fill::NonZero,
                    vello::peniko::BlendMode::default(),
                    1.0,
                    ctx.transform,
                    &Rect::new(row.x0, row.y0, text_right.max(row.x0), row.y1),
                );
            }
            Self::draw_unified_row_content(ctx, line, x0, y);
            if line.kind == DiffLineKind::Hunk {
                ctx.scene.pop_layer();
            }
        }
        buttons
    }

    /// The row's tinted background, and — on a hunk's first row — its
    /// stage/unstage (+ worktree-side discard) buttons (part of
    /// [`Self::draw_unified`]).
    fn draw_unified_row_background(
        ctx: &mut DiffPaintCtx<'_>,
        row: Rect,
        line: &DiffLine,
        last_hunk: &mut Option<usize>,
        buttons: &mut Vec<HunkButton>,
    ) {
        let scene = &mut *ctx.scene;
        let t = ctx.transform;
        let theme = ctx.theme;
        let c = &theme.tokens.colors;
        match line.kind {
            DiffLineKind::Addition => draw::fill_rect(scene, t, row, draw::add_tint(theme)),
            DiffLineKind::Deletion => draw::fill_rect(scene, t, row, draw::del_tint(theme)),
            DiffLineKind::Hunk => {
                draw::fill_rect(scene, t, row, c.surfaces.raised);
                // Emit the hunk action button(s) on the first row of the
                // hunk: always a stage/unstage toggle, plus a discard button
                // on the worktree (unstaged) side.
                if Some(line.hunk) != *last_hunk {
                    let hovered = ctx.interaction.hovered == Some(line.hunk);
                    let toggle_label = if ctx.interaction.staged_side {
                        "Unstage hunk"
                    } else {
                        "Stage hunk"
                    };
                    let mut right = row.x1 - 8.0;
                    let toggle = Self::draw_hunk_button(
                        ctx,
                        &HunkButtonSpec {
                            row,
                            right_x: right,
                            hunk: line.hunk,
                            action: HunkAction::Toggle,
                            label: toggle_label,
                            hovered,
                        },
                    );
                    right = toggle.rect.x0 - 8.0;
                    buttons.push(toggle);
                    if !ctx.interaction.staged_side {
                        let armed = ctx.interaction.armed_discard == Some(line.hunk);
                        let dlabel = if armed { "Discard?" } else { "Discard" };
                        let discard = Self::draw_hunk_button(
                            ctx,
                            &HunkButtonSpec {
                                row,
                                right_x: right,
                                hunk: line.hunk,
                                action: HunkAction::Discard,
                                label: dlabel,
                                hovered: hovered || armed,
                            },
                        );
                        buttons.push(discard);
                    }
                    *last_hunk = Some(line.hunk);
                }
            }
            DiffLineKind::Meta => draw::fill_rect(scene, t, row, c.surfaces.raised),
            DiffLineKind::Context => {}
        }
    }

    /// The line-number gutter, marker glyph, and syntax-highlighted (or
    /// plain) content for one row (part of [`Self::draw_unified`]).
    fn draw_unified_row_content(ctx: &mut DiffPaintCtx<'_>, line: &DiffLine, x0: f64, y: f64) {
        Self::draw_linenos(ctx, x0, y, line);
        let scene = &mut *ctx.scene;
        let tr = &mut *ctx.text;
        let t = ctx.transform;
        let theme = ctx.theme;
        let c = &theme.tokens.colors;

        // Marker glyph + content.
        let content_x = x0 + GUTTER_W;
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
        draw::mono(scene, tr, t, marker, FONT, marker_color, content_x, y + 2.0);

        let text_x = content_x + 14.0;
        match line.kind {
            DiffLineKind::Hunk => {
                draw::mono(
                    scene,
                    tr,
                    t,
                    &line.content,
                    FONT,
                    c.semantic.info,
                    x0 + 8.0,
                    y + 2.0,
                );
            }
            DiffLineKind::Meta => {
                draw::mono(
                    scene,
                    tr,
                    t,
                    &line.content,
                    FONT,
                    c.text_dim,
                    content_x,
                    y + 2.0,
                );
            }
            _ => Self::draw_code(
                ctx,
                &line.content,
                &ctx.diff.language,
                text_x,
                y + 2.0,
                ctx.settings.syntax_highlight,
            ),
        }
    }

    fn draw_split(ctx: &mut DiffPaintCtx<'_>, scroll: f64) -> Vec<HunkButton> {
        let viewport = ctx.viewport;
        let border = ctx.theme.tokens.colors.border_standard;
        let raised = ctx.theme.tokens.colors.surfaces.raised;
        let add_tint = draw::add_tint(ctx.theme);
        let del_tint = draw::del_tint(ctx.theme);
        let mid = f64::midpoint(viewport.x0, viewport.x1);
        draw::vline(
            ctx.scene,
            ctx.transform,
            mid,
            viewport.y0,
            viewport.y1,
            border,
        );

        let top = viewport.y0 - scroll;
        // Pair deletions/additions into aligned rows. We walk lines and
        // emit left (old) / right (new) cells; context spans both.
        let rows = pair_rows(&ctx.diff.lines);
        for (i, pr) in rows.iter().enumerate() {
            let y = top + i as f64 * ROW_H;
            if y + ROW_H < viewport.y0 || y > viewport.y1 {
                continue;
            }
            let left = Rect::new(viewport.x0, y, mid, y + ROW_H);
            let right = Rect::new(mid, y, viewport.x1, y + ROW_H);

            // `pr.left`/`pr.right` are always valid indices into this same
            // `diff.lines` (`pair_rows` builds them from it), but `.get`
            // degrades to skipping the half-row instead of indexing blindly
            // into text parsed from a repository someone else wrote.
            if let Some(li) = pr.left
                && let Some(line) = ctx.diff.lines.get(li)
            {
                if line.kind == DiffLineKind::Deletion {
                    draw::fill_rect(ctx.scene, ctx.transform, left, del_tint);
                } else if line.kind == DiffLineKind::Hunk || line.kind == DiffLineKind::Meta {
                    draw::fill_rect(ctx.scene, ctx.transform, left, raised);
                }
                Self::draw_half(ctx, line, viewport.x0, y, true);
            }
            if let Some(ri) = pr.right
                && let Some(line) = ctx.diff.lines.get(ri)
            {
                if line.kind == DiffLineKind::Addition {
                    draw::fill_rect(ctx.scene, ctx.transform, right, add_tint);
                } else if line.kind == DiffLineKind::Hunk || line.kind == DiffLineKind::Meta {
                    draw::fill_rect(ctx.scene, ctx.transform, right, raised);
                }
                Self::draw_half(ctx, line, mid, y, false);
            }
        }
        Vec::new()
    }

    #[allow(
        clippy::many_single_char_names,
        reason = "t/y/c follow this file's graphics convention (transform, position, colour tokens); n/s/w are the line number and its formatted string/measured width"
    )]
    fn draw_half(ctx: &mut DiffPaintCtx<'_>, line: &DiffLine, x0: f64, y: f64, is_left: bool) {
        let scene = &mut *ctx.scene;
        let tr = &mut *ctx.text;
        let t = ctx.transform;
        let theme = ctx.theme;
        let c = &theme.tokens.colors;
        let no = if is_left {
            line.old_lineno
        } else {
            line.new_lineno
        };
        if let Some(n) = no {
            let s = n.to_string();
            let w = f64::from(draw::mono_width(tr, &s, FONT));
            draw::mono(scene, tr, t, &s, FONT, c.text_dim, x0 + 44.0 - w, y + 2.0);
        }
        let text_x = x0 + 54.0;
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
                    y + 2.0,
                );
            }
            DiffLineKind::Meta => {
                draw::mono(
                    scene,
                    tr,
                    t,
                    &line.content,
                    FONT,
                    c.text_dim,
                    text_x,
                    y + 2.0,
                );
            }
            _ => Self::draw_code(
                ctx,
                &line.content,
                "text",
                text_x,
                y + 2.0,
                ctx.settings.syntax_highlight,
            ),
        }
    }

    fn draw_linenos(ctx: &mut DiffPaintCtx<'_>, x0: f64, y: f64, line: &DiffLine) {
        let scene = &mut *ctx.scene;
        let tr = &mut *ctx.text;
        let t = ctx.transform;
        let theme = ctx.theme;
        let c = &theme.tokens.colors;
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
            x0 + 40.0 - ow,
            y + 2.0,
        );
        draw::mono(
            scene,
            tr,
            t,
            &new,
            FONT,
            c.text_dim,
            x0 + 84.0 - nw,
            y + 2.0,
        );
        draw::vline(
            scene,
            t,
            x0 + GUTTER_W - 4.0,
            y,
            y + ROW_H,
            c.border_standard,
        );
    }

    fn draw_code(
        ctx: &mut DiffPaintCtx<'_>,
        content: &str,
        language: &str,
        x: f64,
        y: f64,
        highlight: bool,
    ) {
        let scene = &mut *ctx.scene;
        let tr = &mut *ctx.text;
        let t = ctx.transform;
        let theme = ctx.theme;
        let c = &theme.tokens.colors;
        if !highlight {
            draw::mono(scene, tr, t, content, FONT, c.text_primary, x, y);
            return;
        }
        let mut cx = x;
        for tok in tokenize(content, language) {
            let color = token_color(theme, tok.class);
            draw::mono(scene, tr, t, &tok.text, FONT, color, cx, y);
            cx += f64::from(draw::mono_width(tr, &tok.text, FONT));
        }
    }

    /// Draw one hunk-action button whose right edge sits at `right_x`, and
    /// return its hit rect. Discard buttons read in the danger hue when hovered
    /// so the destructive action is visually distinct from stage/unstage.
    fn draw_hunk_button(ctx: &mut DiffPaintCtx<'_>, spec: &HunkButtonSpec<'_>) -> HunkButton {
        let scene = &mut *ctx.scene;
        let tr = &mut *ctx.text;
        let t = ctx.transform;
        let theme = ctx.theme;
        let c = &theme.tokens.colors;
        let size = 11.0f32;
        let tw = f64::from(
            tr.measure_weighted(spec.label, size, None, TextWeight::Medium)
                .0,
        );
        let bw = tw + 20.0;
        let bx = spec.right_x - bw;
        let by = spec.row.y0 + 2.0;
        let brect = Rect::new(bx, by, bx + bw, spec.row.y1 - 2.0);
        let accent = match spec.action {
            HunkAction::Toggle => theme.accent_primary,
            HunkAction::Discard => c.semantic.danger,
        };
        let bg = if spec.hovered { accent } else { c.lift(0.72) };
        let fg = if spec.hovered {
            Color::WHITE
        } else {
            c.text_dim
        };
        draw::fill_round(scene, t, brect, 5.0, bg);
        draw::text_weighted(
            scene,
            tr,
            t,
            spec.label,
            size,
            fg,
            bx + (bw - tw) / 2.0,
            by + (ROW_H - 4.0 - f64::from(size)) / 2.0,
            None,
            TextWeight::Medium,
        );
        HunkButton {
            hunk: spec.hunk,
            action: spec.action,
            rect: brect,
        }
    }
}

fn token_color(theme: &Theme, class: TokenClass) -> Color {
    let c = &theme.tokens.colors;
    match class {
        TokenClass::Plain => c.text_primary,
        TokenClass::Keyword => c.accent[6],
        TokenClass::Str => c.semantic.success,
        TokenClass::Comment => c.text_dim,
        TokenClass::Number => c.semantic.warning,
        TokenClass::Type => c.semantic.info,
        TokenClass::Punct => c.neutral[7],
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

    fn hunk_buttons(staged_side: bool) -> Vec<HunkButton> {
        let diff = parse_unified("x.rs", "rust", "@@ -1,1 +1,1 @@\n-old\n+new\n");
        let mut scene = Scene::new();
        let mut text = TextRenderer::new();
        let mut paint = draw::PaintCtx::new(&mut scene, &mut text, Affine::IDENTITY);
        DiffView::draw(
            &mut paint,
            &Theme::default(),
            Rect::new(0.0, 0.0, 640.0, 320.0),
            &diff,
            0.0,
            &Settings::default(),
            HunkInteraction {
                staged_side,
                ..HunkInteraction::default()
            },
        )
    }

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
        assert!(DiffView::content_height(&d) > ROW_H * 2.0);
    }

    #[test]
    fn hunk_actions_match_the_side_the_diff_came_from() {
        let worktree = hunk_buttons(false);
        assert_eq!(worktree.len(), 2, "worktree hunks can stage or discard");
        assert!(
            worktree
                .iter()
                .any(|button| button.action == HunkAction::Toggle)
        );
        assert!(
            worktree
                .iter()
                .any(|button| button.action == HunkAction::Discard)
        );

        let staged = hunk_buttons(true);
        assert_eq!(staged.len(), 1, "index hunks can only be unstaged");
        assert_eq!(staged[0].action, HunkAction::Toggle);
    }
}
