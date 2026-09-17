//! The changes column: staged / unstaged / untracked file lists with
//! per-row stage / unstage affordances and a commit box at the bottom.
//!
//! Rendered into a fixed-width left column of the changes screen. Each
//! row is hit-testable (select the file → show its diff; click the
//! stage/unstage chevron → move it between areas). The commit box shows
//! the author identity and a commit button whose enabled state tracks the
//! staged count and message length.

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Color;

use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;

use crate::draw;
use crate::model::{ChangeArea, FileChange, RepoModel};

const ROW_H: f64 = 30.0;
const SECTION_H: f64 = 30.0;
const COMMIT_BOX_H: f64 = 150.0;
/// Height of the "— nothing here —" band an empty section paints.
const EMPTY_HINT_H: f64 = 22.0;
/// Left/right inset of section titles and bulk actions.
const GUTTER: f64 = 14.0;
/// Height of the section count badge (`draw::pill`).
const BADGE_H: f64 = 16.0;
/// Chevron button: square-ish, inset from the row's right edge.
const CHEVRON_W: f64 = 20.0;
const CHEVRON_INSET: f64 = 8.0;
/// Gap between the +/- counts and the chevron button.
const COUNTS_GAP: f64 = 8.0;

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
        area: Rect,
        y: f64,
        f: &FileChange,
        is_staged: bool,
    ) -> Self {
        let badge = f.kind.badge();
        let badge_x = area.x0 + GUTTER;
        let name_x = area.x0 + 32.0;
        let chevron_rect = Rect::new(
            area.x1 - CHEVRON_INSET - CHEVRON_W,
            y + 4.0,
            area.x1 - CHEVRON_INSET,
            y + ROW_H - 4.0,
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

/// What the user clicked in the changes column.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChangesHit {
    /// Select a file to view its diff. `(area, index-within-area)`.
    SelectFile(ChangeArea, usize),
    /// Toggle staging on a single file.
    ToggleStage(ChangeArea, usize),
    /// Bulk action header buttons.
    StageAll,
    UnstageAll,
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
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        area: Rect,
        model: &RepoModel,
        selected: Option<(ChangeArea, usize)>,
        scroll: f64,
        message: &str,
    ) -> ChangesDraw {
        let c = &theme.tokens.colors;
        draw::fill_rect(scene, t, area, c.surfaces.raised);
        draw::vline(scene, t, area.x1, area.y0, area.y1, c.border_standard);

        let list_area = Rect::new(area.x0, area.y0, area.x1, area.y1 - COMMIT_BOX_H);
        let mut regions: Vec<HitRegion> = Vec::new();

        // Clip the scrollable list.
        scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            t,
            &list_area,
        );

        let mut y = list_area.y0 - scroll;
        y = Self::section(
            scene,
            tr,
            t,
            theme,
            list_area,
            "Staged Changes",
            &model.staged,
            ChangeArea::Staged,
            selected,
            y,
            true,
            &mut regions,
        );
        y = Self::section(
            scene,
            tr,
            t,
            theme,
            list_area,
            "Unstaged Changes",
            &model.unstaged,
            ChangeArea::Unstaged,
            selected,
            y,
            false,
            &mut regions,
        );
        let _ = Self::section(
            scene,
            tr,
            t,
            theme,
            list_area,
            "Untracked Files",
            &model.untracked,
            ChangeArea::Untracked,
            selected,
            y,
            false,
            &mut regions,
        );

        scene.pop_layer();

        // Commit box pinned to the bottom. Returns the rect the owner uses to
        // position the commit-message TextInput.
        let commit_input =
            Self::commit_box(scene, tr, t, theme, area, model, message, &mut regions);

        // Build closures that resolve a click to a hit. Returned as a
        // boxed list so the app can route without knowing geometry.
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

    #[allow(clippy::too_many_arguments)]
    fn section(
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        area: Rect,
        title: &str,
        files: &[FileChange],
        change_area: ChangeArea,
        selected: Option<(ChangeArea, usize)>,
        y: f64,
        is_staged: bool,
        regions: &mut Vec<HitRegion>,
    ) -> f64 {
        let mut y = Self::section_header(
            scene, tr, t, theme, area, title, files, y, is_staged, regions,
        );

        if files.is_empty() {
            let c = &theme.tokens.colors;
            let hint = "— nothing here —";
            let ty_1 = draw::centered_top(tr, hint, 12.0, TextWeight::Regular, y, EMPTY_HINT_H);
            draw::text(
                scene,
                tr,
                t,
                hint,
                12.0,
                c.text_dim,
                area.x0 + 20.0,
                ty_1,
                None,
            );
            return y + EMPTY_HINT_H;
        }

        for (i, f) in files.iter().enumerate() {
            Self::section_row(
                scene,
                tr,
                t,
                theme,
                area,
                change_area,
                selected,
                y,
                is_staged,
                f,
                i,
                regions,
            );
            y += ROW_H;
        }
        y + 8.0
    }

    /// The section header: title, count badge, and (when non-empty) the
    /// bulk stage/unstage-all action (part of [`Self::section`]). Returns
    /// the `y` the file rows should start at.
    #[allow(
        clippy::too_many_arguments,
        reason = "internal draw-step split of an over-length function"
    )]
    fn section_header(
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        area: Rect,
        title: &str,
        files: &[FileChange],
        y: f64,
        is_staged: bool,
        regions: &mut Vec<HitRegion>,
    ) -> f64 {
        let c = &theme.tokens.colors;
        // Title, count badge and bulk action all centre on the header band:
        // each is placed from its own measured line height rather than a
        // per-element nudge, so they read as one row.
        let (title_w, _) = tr.measure_weighted(title, 12.0, None, TextWeight::Semibold);
        let ty_2 = draw::centered_top(tr, title, 12.0, TextWeight::Semibold, y, SECTION_H);
        draw::text_weighted(
            scene,
            tr,
            t,
            title,
            12.0,
            c.text_dim,
            area.x0 + GUTTER,
            ty_2,
            None,
            TextWeight::Semibold,
        );
        // Count badge.
        let count = files.len().to_string();
        let cx = area.x0 + GUTTER + f64::from(title_w) + 8.0;
        draw::pill(
            scene,
            tr,
            t,
            cx,
            y + (SECTION_H - BADGE_H) / 2.0,
            BADGE_H,
            &count,
            c.text_primary,
            c.lift(0.72),
            None,
        );

        if !files.is_empty() {
            let label = if is_staged {
                "Unstage all"
            } else {
                "Stage all"
            };
            let lw = f64::from(tr.measure_weighted(label, 11.0, None, TextWeight::Medium).0);
            let lx = area.x1 - lw - GUTTER;
            let ty_3 = draw::centered_top(tr, label, 11.0, TextWeight::Medium, y, SECTION_H);
            draw::text_weighted(
                scene,
                tr,
                t,
                label,
                11.0,
                theme.accent_primary,
                lx,
                ty_3,
                None,
                TextWeight::Medium,
            );
            regions.push(HitRegion {
                rect: Rect::new(lx - 4.0, y, area.x1, y + SECTION_H),
                hit: if is_staged {
                    ChangesHit::UnstageAll
                } else {
                    ChangesHit::StageAll
                },
            });
        }
        y + SECTION_H
    }

    /// One file row: selection tint, status badge, name/dir, +/- counts,
    /// and the stage/unstage chevron button (part of [`Self::section`]).
    #[allow(
        clippy::too_many_arguments,
        reason = "internal draw-step split of an over-length function"
    )]
    #[allow(
        clippy::many_single_char_names,
        reason = "t/y/c follow this file's graphics convention (transform, position, colour tokens); f/i are the iterated FileChange and its index"
    )]
    fn section_row(
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        area: Rect,
        change_area: ChangeArea,
        selected: Option<(ChangeArea, usize)>,
        y: f64,
        is_staged: bool,
        f: &FileChange,
        i: usize,
        regions: &mut Vec<HitRegion>,
    ) {
        let c = &theme.tokens.colors;
        let rrect = Rect::new(area.x0, y, area.x1, y + ROW_H);
        let is_sel = selected == Some((change_area, i));
        if is_sel {
            // A quiet surface wash keeps selection discoverable without
            // turning a whole row into a saturated accent block. The accent
            // rail below remains the strong, stable selection cue.
            draw::fill_rect(scene, t, rrect, c.lift(0.72));
            draw::fill_rect(
                scene,
                t,
                Rect::new(area.x0, y, area.x0 + 3.0, y + ROW_H),
                theme.accent_primary,
            );
        }

        // Everything in a row sits on one centre line. The +/- counts used to
        // be a second text line at `y + 18` inside the 30px row, which ran
        // into the row below; they now sit right-aligned beside the chevron.
        let layout = RowLayout::measure(tr, area, y, f, is_staged);

        // Status badge.
        let bcolor = draw::kind_color(theme, f.kind);
        let ty_4 = draw::mono_centered_top(tr, layout.badge, 12.0, y, ROW_H);
        draw::mono(
            scene,
            tr,
            t,
            layout.badge,
            12.0,
            bcolor,
            layout.badge_x,
            ty_4,
        );

        // Name (bold) + dir (dim). Elide to the allocation: a wrap width
        // would break a long name onto a second line inside the 30px row.
        let name = tr.elide_to_width_weighted(
            f.name(),
            12.5,
            layout.name_max_w as f32,
            TextWeight::Medium,
        );
        let name_y = draw::centered_top(tr, &name, 12.5, TextWeight::Medium, y, ROW_H);
        draw::text_weighted(
            scene,
            tr,
            t,
            &name,
            12.5,
            c.text_primary,
            layout.name_x,
            name_y,
            None,
            TextWeight::Medium,
        );
        let dir = f.dir();
        if !dir.is_empty() && layout.dir_max_w > 0.0 {
            let dir = tr.elide_to_width(dir, 11.0, layout.dir_max_w as f32);
            let ty_6 = draw::centered_top(tr, &dir, 11.0, TextWeight::Regular, y, ROW_H);
            draw::text(
                scene,
                tr,
                t,
                &dir,
                11.0,
                c.text_dim,
                layout.dir_x,
                ty_6,
                None,
            );
        }

        // +/- counts, right-aligned against the chevron.
        if let Some(counts) = &layout.counts {
            let ty_7 = draw::mono_centered_top(tr, counts, 11.0, y, ROW_H);
            draw::mono(
                scene,
                tr,
                t,
                counts,
                11.0,
                c.text_dim,
                layout.counts_x,
                ty_7,
            );
        }

        // Stage / unstage chevron button on the right.
        let chev = layout.chevron;
        let brect = layout.chevron_rect;
        draw::fill_round(scene, t, brect, 4.0, c.lift(0.72));
        let chw = f64::from(draw::mono_width(tr, chev, 14.0));
        let chev_y = draw::mono_centered_top(tr, chev, 14.0, brect.y0, brect.height());
        draw::mono(
            scene,
            tr,
            t,
            chev,
            14.0,
            if is_staged {
                draw::del_fg(theme)
            } else {
                draw::add_fg(theme)
            },
            brect.x0 + (brect.width() - chw) / 2.0,
            chev_y,
        );
        regions.push(HitRegion {
            rect: brect,
            hit: ChangesHit::ToggleStage(change_area, i),
        });

        // Row select region (everything left of the chevron).
        regions.push(HitRegion {
            rect: Rect::new(area.x0, y, brect.x0 - 2.0, y + ROW_H),
            hit: ChangesHit::SelectFile(change_area, i),
        });
    }

    #[allow(clippy::too_many_arguments)]
    fn commit_box(
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        area: Rect,
        model: &RepoModel,
        message: &str,
        regions: &mut Vec<HitRegion>,
    ) -> Rect {
        let c = &theme.tokens.colors;
        let box_rect = Rect::new(area.x0, area.y1 - COMMIT_BOX_H, area.x1, area.y1);
        draw::fill_rect(scene, t, box_rect, c.surfaces.base);
        draw::hline(
            scene,
            t,
            box_rect.y0,
            box_rect.x0,
            box_rect.x1,
            c.border_standard,
        );

        let ay = box_rect.y0 + 12.0;
        Self::commit_box_author(scene, tr, t, theme, box_rect, model, ay);

        // Message input area. The owning app positions + draws a toolkit
        // TextInput here (it paints its own frame, text and caret), and a
        // click in this rect focuses it — so we only reserve the rect and its
        // focus region here.
        let input = Rect::new(
            box_rect.x0 + 12.0,
            ay + 32.0,
            box_rect.x1 - 12.0,
            box_rect.y1 - 52.0,
        );
        regions.push(HitRegion {
            rect: input,
            hit: ChangesHit::FocusMessage,
        });

        Self::commit_box_button(scene, tr, t, theme, box_rect, model, message, regions);

        input
    }

    /// The author identity row (avatar + name/email) (part of
    /// [`Self::commit_box`]).
    fn commit_box_author(
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        box_rect: Rect,
        model: &RepoModel,
        ay: f64,
    ) {
        let c = &theme.tokens.colors;
        let ax = box_rect.x0 + 12.0;
        draw::avatar(
            scene,
            tr,
            t,
            theme,
            ax,
            ay,
            22.0,
            &model.author_name,
            draw::author_color(theme, &model.author_email),
        );
        let who = if model.author_name.is_empty() {
            "Set git identity".to_string()
        } else {
            format!("{} <{}>", model.author_name, model.author_email)
        };
        draw::text(
            scene,
            tr,
            t,
            &who,
            11.5,
            c.text_dim,
            ax + 30.0,
            ay + 4.0,
            Some((box_rect.width() - 60.0) as f32),
        );
    }

    /// The commit button and the branch indicator beside it (part of
    /// [`Self::commit_box`]).
    #[allow(
        clippy::too_many_arguments,
        reason = "internal draw-step split of an over-length function"
    )]
    fn commit_box_button(
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        box_rect: Rect,
        model: &RepoModel,
        message: &str,
        regions: &mut Vec<HitRegion>,
    ) {
        let c = &theme.tokens.colors;
        let enabled = !model.staged.is_empty() && !message.trim().is_empty();
        let brect = Rect::new(
            box_rect.x1 - 150.0,
            box_rect.y1 - 42.0,
            box_rect.x1 - 12.0,
            box_rect.y1 - 12.0,
        );
        let bg = if enabled {
            theme.accent_primary
        } else {
            c.lift(0.72)
        };
        let fg = if enabled { Color::WHITE } else { c.neutral[5] };
        draw::fill_round(scene, t, brect, 6.0, bg);
        let staged_n = model.staged.len();
        let label = if staged_n == 0 {
            "Commit".to_string()
        } else {
            format!(
                "Commit {staged_n} file{}",
                if staged_n == 1 { "" } else { "s" }
            )
        };
        let (lw, lh) = tr.measure_weighted(&label, 12.0, None, TextWeight::Semibold);
        draw::text_weighted(
            scene,
            tr,
            t,
            &label,
            12.0,
            fg,
            brect.x0 + (brect.width() - f64::from(lw)) / 2.0,
            brect.y0 + (brect.height() - f64::from(lh)) / 2.0,
            None,
            TextWeight::Semibold,
        );
        if enabled {
            regions.push(HitRegion {
                rect: brect,
                hit: ChangesHit::Commit,
            });
        }

        // Branch indicator on the left of the button row.
        draw::text(
            scene,
            tr,
            t,
            &format!("on {}", model.head),
            11.5,
            c.text_dim,
            box_rect.x0 + 12.0,
            box_rect.y1 - 34.0,
            None,
        );
    }
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

    #[test]
    fn row_layout_keeps_name_dir_counts_and_chevron_in_one_line() {
        let mut tr = TextRenderer::new();
        let area = Rect::new(0.0, 0.0, 320.0, 800.0);
        let y = 86.0;
        let file = change("src/render/a-fairly-long-file-name.rs", 124, 6);
        let layout = RowLayout::measure(&mut tr, area, y, &file, false);
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

    #[test]
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
        let layout = RowLayout::measure(&mut tr, area, 0.0, &file, false);
        assert!(layout.counts.is_none());
        assert!(
            (layout.name_x + layout.name_max_w - (layout.chevron_rect.x0 - COUNTS_GAP)).abs() < 0.01,
            "text runs up to the chevron gap when there are no counts"
        );
    }
}
