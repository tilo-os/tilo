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
        ChangesDraw { hits, commit_input }
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
        mut y: f64,
        is_staged: bool,
        regions: &mut Vec<HitRegion>,
    ) -> f64 {
        let c = &theme.tokens.colors;
        // Section header with count + bulk action.
        draw::text_weighted(
            scene,
            tr,
            t,
            title,
            12.0,
            c.text_dim,
            area.x0 + 14.0,
            y + 8.0,
            None,
            TextWeight::Semibold,
        );
        // Count badge.
        let count = files.len().to_string();
        let cx = area.x0
            + 14.0
            + f64::from(
                tr.measure_weighted(title, 12.0, None, TextWeight::Semibold)
                    .0,
            )
            + 8.0;
        draw::pill(
            scene,
            tr,
            t,
            cx,
            y + 6.0,
            16.0,
            &count,
            c.text_primary,
            c.surface_white_10,
            None,
        );

        if !files.is_empty() {
            let label = if is_staged {
                "Unstage all"
            } else {
                "Stage all"
            };
            let lw = f64::from(tr.measure_weighted(label, 11.0, None, TextWeight::Medium).0);
            let lx = area.x1 - lw - 14.0;
            draw::text_weighted(
                scene,
                tr,
                t,
                label,
                11.0,
                theme.accent_primary,
                lx,
                y + 9.0,
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
        y += SECTION_H;

        if files.is_empty() {
            draw::text(
                scene,
                tr,
                t,
                "— nothing here —",
                12.0,
                c.neutral[5],
                area.x0 + 20.0,
                y + 2.0,
                None,
            );
            return y + 22.0;
        }

        for (i, f) in files.iter().enumerate() {
            let rrect = Rect::new(area.x0, y, area.x1, y + ROW_H);
            let is_sel = selected == Some((change_area, i));
            if is_sel {
                draw::fill_rect(scene, t, rrect, c.accent[2]);
                draw::fill_rect(
                    scene,
                    t,
                    Rect::new(area.x0, y, area.x0 + 3.0, y + ROW_H),
                    theme.accent_primary,
                );
            }

            // Status badge.
            let badge = f.kind.badge();
            let bcolor = draw::kind_color(theme, f.kind);
            draw::mono(scene, tr, t, badge, 12.0, bcolor, area.x0 + 14.0, y + 8.0);

            // Name (bold) + dir (dim).
            let name = f.name();
            draw::text_weighted(
                scene,
                tr,
                t,
                name,
                12.5,
                c.text_primary,
                area.x0 + 32.0,
                y + 7.0,
                None,
                TextWeight::Medium,
            );
            let nw = f64::from(tr.measure_weighted(name, 12.5, None, TextWeight::Medium).0);
            let dir = f.dir();
            if !dir.is_empty() {
                draw::text(
                    scene,
                    tr,
                    t,
                    dir,
                    11.0,
                    c.neutral[5],
                    area.x0 + 36.0 + nw,
                    y + 8.0,
                    Some((area.width() - 130.0 - nw) as f32),
                );
            }

            // +/- counts.
            if f.additions > 0 || f.deletions > 0 {
                let counts = format!("+{} -{}", f.additions, f.deletions);
                draw::mono(
                    scene,
                    tr,
                    t,
                    &counts,
                    10.5,
                    c.text_dim,
                    area.x0 + 36.0 + nw,
                    y + 18.0,
                );
            }

            // Stage / unstage chevron button on the right.
            let chev = if is_staged { "−" } else { "+" };
            let brect = Rect::new(area.x1 - 28.0, y + 4.0, area.x1 - 8.0, y + ROW_H - 4.0);
            draw::fill_round(scene, t, brect, 4.0, c.surface_white_10);
            let chw = f64::from(draw::mono_width(tr, chev, 14.0));
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
                brect.y0 + 1.0,
            );
            regions.push(HitRegion {
                rect: brect,
                hit: ChangesHit::ToggleStage(change_area, i),
            });

            // Row select region (everything left of the chevron).
            regions.push(HitRegion {
                rect: Rect::new(area.x0, y, area.x1 - 30.0, y + ROW_H),
                hit: ChangesHit::SelectFile(change_area, i),
            });

            y += ROW_H;
        }
        y + 8.0
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

        // Author identity row.
        let ax = box_rect.x0 + 12.0;
        let ay = box_rect.y0 + 12.0;
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

        // Commit button.
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
            c.surface_white_10
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
        let lw = f64::from(
            tr.measure_weighted(&label, 12.0, None, TextWeight::Semibold)
                .0,
        );
        draw::text_weighted(
            scene,
            tr,
            t,
            &label,
            12.0,
            fg,
            brect.x0 + (brect.width() - lw) / 2.0,
            brect.y0 + 7.0,
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

        input
    }
}
