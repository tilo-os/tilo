//! The commit page: what a commit changed, and the diffs of the files it
//! touched.
//!
//! History is read, not edited: a commit's page names its files, and one of
//! them opens in the same diff view the working tree uses. The files are read
//! the first time a commit is opened, because walking every commit's tree up
//! front costs a second on a real repository.
//!
//! A conflicted file is the other half of this: it is the only kind of change
//! whose *decision* changes the repository, so its two sides are offered
//! above its diff rather than left to a terminal.

use vello::kurbo::{Point, Rect};

use ui::paint::PaintCtx;
use ui::tokens::TextRole;
use ui::widget::{ButtonFace, ControlSize, EmptyState, EmptyStateContent};

use crate::app::change_list;
use crate::diffview;
use crate::history;
use crate::i18n::{t, t_args};

use crate::diffview::DiffHit;

use super::{GitClientApp, Hit, ScrollRegion, Scroller, Target};

/// What the band above a conflicted file says: what each choice costs, in
/// the person's terms, or — where the sides cannot be read — the bare fact
/// that both changed the file.
pub(super) fn conflict_note(cost: Option<crate::repo::ConflictCost>) -> String {
    match cost {
        Some(cost) => t_args(
            "git-conflict-cost",
            &[("ours", cost.ours.into()), ("theirs", cost.theirs.into())],
        ),
        None => t("git-conflict-explainer"),
    }
}

impl GitClientApp {
    /// Show commit `index`: its own page, with its files and no file diff.
    pub(super) fn select_commit(&mut self, index: usize) {
        if self.selected_commit != index {
            self.selected_commit = index;
            self.file_selection = None;
            self.commit_diff = None;
            self.diff_scroll = 0.0;
            self.detail_scroll = 0.0;
        }
    }

    /// Show the diff of file `index` in the selected commit.
    pub(super) fn open_commit_file(&mut self, index: usize) {
        let Some(commit) = self.model.commits.get(self.selected_commit) else {
            return;
        };
        let commit_id = commit.id.clone();
        self.load_commit_files(&commit_id);
        let Some(file) = self
            .commit_files
            .get(&commit_id)
            .and_then(|files| files.get(index))
            .map(|file| file.path.clone())
        else {
            return;
        };
        self.commit_diff = Some((commit_id, file));
        self.diff_scroll = 0.0;
        self.diff_hscroll = 0.0;
    }

    /// Whether the file under review is a conflict, so its sides can be
    /// chosen.
    pub(super) fn selected_conflict(&self) -> Option<String> {
        let (area, index) = self.selected_change?;
        change_list(&self.model, area)
            .get(index)
            .filter(|file| file.kind == crate::model::ChangeKind::Conflicted)
            .map(|file| file.path.clone())
    }

    /// Keep one side of the conflicted file under review, which stages it as
    /// resolved, and move on to the next file so a merge with several
    /// conflicts is a few keystrokes rather than a terminal.
    pub(super) fn resolve_conflict(&mut self, side: crate::repo::ConflictSide) {
        let Some(file) = self.selected_conflict() else {
            return;
        };
        let Some(path) = self.repo() else { return };
        match crate::repo::resolve_conflict(&path, &file, side) {
            Ok(()) => {
                self.last_error = None;
                self.forget_conflict_cost();
                let side_word = t(match side {
                    crate::repo::ConflictSide::Ours => "git-conflict-ours",
                    crate::repo::ConflictSide::Theirs => "git-conflict-theirs",
                });
                self.notice = Some(t_args(
                    "git-conflict-resolved",
                    &[
                        ("file", file.as_str().into()),
                        ("side", ui::i18n::ArgValue::from(side_word)),
                    ],
                ));
                self.reload();
            }
            Err(error) => self.report_error(&error),
        }
    }

    /// A conflicted file's sides, above its diff: the choice that resolves
    /// the conflict without a terminal, and what each choice costs.
    ///
    /// The sentence and the two buttons share a row where they fit; in a
    /// narrow window the sentence wraps to the width it has and the buttons
    /// stand under it, because a sentence elided before it says what the
    /// second choice costs is not a sentence about the decision.
    pub(super) fn paint_conflict_actions(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        body: Rect,
        file: &str,
        pointer: Option<Point>,
    ) -> (Rect, Vec<(Rect, Hit)>) {
        let theme = ctx.theme;
        let s = &theme.tokens.spacing;
        let style = theme.text(TextRole::Body);
        self.conflict_cost_for(file);
        let note = self.conflict_sentence();
        let note_w = f64::from(ctx.text.measure_style(&note, None, &style).0);
        let pad = f64::from(s.s3);
        let row_h = ui::widget::ButtonFace::height(ControlSize::Regular, theme)
            .max(f64::from(ctx.text.measure_style("Ag", None, &style).1))
            + 2.0 * f64::from(s.s2);
        let labels = [
            (t("git-conflict-take-ours"), crate::repo::ConflictSide::Ours),
            (
                t("git-conflict-take-theirs"),
                crate::repo::ConflictSide::Theirs,
            ),
        ];
        let widths: Vec<f64> = labels
            .iter()
            .map(|(label, _)| {
                ui::widget::ButtonFace::new(Rect::ZERO, label).intrinsic_width(ctx.text, theme)
            })
            .collect();
        let buttons_w: f64 = widths.iter().sum::<f64>()
            + f64::from(s.s2) * (f64::from(u32::try_from(widths.len()).unwrap_or(1)) - 1.0);
        // One row when the sentence has room beside the buttons, two when it
        // has not.
        let side_by_side = pad + note_w + f64::from(s.s4) + buttons_w + pad <= body.width();
        let text_room = if side_by_side {
            note_w
        } else {
            (body.width() - 2.0 * pad).max(0.0)
        };
        let text_h = if side_by_side {
            row_h
        } else {
            let measured =
                ctx.text
                    .measure_style(&note, Some(ui::num::f32_of_f64(text_room)), &style);
            (f64::from(measured.1) + 2.0 * f64::from(s.s2)).max(row_h)
        };
        let band = Rect::new(
            body.x0,
            body.y0,
            body.x1,
            body.y0 + text_h + row_h * f64::from(u8::from(!side_by_side)),
        );
        ctx.text(
            &note,
            &style,
            theme.text_dim,
            Point::new(band.x0 + pad, band.y0 + f64::from(s.s2)),
            Some(ui::num::f32_of_f64(text_room)),
        );
        // Read out with the buttons: what the choice costs is the choice.
        self.semantic.push(Target {
            rect: Rect::new(band.x0, band.y0, band.x1, band.y0 + text_h),
            hit: Hit::ConflictNote,
        });
        let button_y = if side_by_side {
            band.y0
        } else {
            band.y0 + text_h
        };
        let mut x = if side_by_side {
            band.x0 + pad + note_w + f64::from(s.s4)
        } else {
            band.x0 + pad
        };
        let mut hits = Vec::new();
        for ((label, side), w) in labels.into_iter().zip(widths) {
            // The width comes from the label; the face that is painted is
            // the one with the button's own rect, or nothing shows.
            let rect = Rect::new(
                x,
                button_y + f64::from(s.s1),
                x + w,
                button_y + row_h - f64::from(s.s1),
            );
            let hovered = pointer.is_some_and(|p| rect.contains(p));
            let face = ui::widget::ButtonFace::new(rect, &label).hovered(hovered);
            ui::widget::Button::paint(ctx.scene, ctx.transform, ctx.text, theme, &face);
            hits.push((rect, Hit::ResolveConflict(side)));
            x = rect.x1 + f64::from(s.s2);
        }
        (Rect::new(body.x0, band.y1, body.x1, body.y1), hits)
    }

    /// What keeping each side of `file` would discard. Read once per
    /// conflict — from the repository where there is one, from the bundled
    /// sample's canned sides where the shot shows a repository that is not
    /// on disk — and remembered until another file is picked.
    pub(super) fn conflict_cost_for(&mut self, file: &str) -> Option<crate::repo::ConflictCost> {
        if let Some((known, cost)) = &self.conflict_cost
            && known == file
        {
            return *cost;
        }
        let cost = match &self.repo_path {
            Some(path) => crate::repo::conflict_cost(path, file).ok(),
            None => crate::sample::conflict_cost(file),
        };
        self.conflict_cost = Some((file.to_owned(), cost));
        cost
    }

    /// Forget the read cost: the sides change as soon as one is kept.
    pub(super) fn forget_conflict_cost(&mut self) {
        self.conflict_cost = None;
    }

    /// The sentence above the conflicted file, as the band paints it and as
    /// assistive technology reads it: one sentence, one source.
    pub(super) fn conflict_sentence(&self) -> String {
        let cost = match (&self.conflict_cost, self.selected_conflict()) {
            (Some((known, cost)), Some(file)) if *known == file => *cost,
            _ => None,
        };
        conflict_note(cost)
    }

    /// The selected commit, or one file of what it changed.
    pub(super) fn paint_detail(&mut self, ctx: &mut PaintCtx<'_>, body: Rect) {
        let Some(commit) = self.model.commits.get(self.selected_commit) else {
            EmptyState::paint(
                ctx,
                body,
                &EmptyStateContent::new(&t("git-no-commits")),
                Point::new(f64::MIN, f64::MIN),
            );
            return;
        };
        if let Some((commit_id, file)) = self.commit_diff.clone() {
            self.paint_commit_diff(ctx, body, &commit_id, &file);
            return;
        }
        let mut commit = commit.clone();
        let commit_id = commit.id.clone();
        self.load_commit_files(&commit_id);
        if commit.files.is_empty() {
            commit.files = self
                .commit_files
                .get(&commit_id)
                .cloned()
                .unwrap_or_default();
        }
        let draw = history::paint_detail(ctx, body, &commit, self.detail_scroll);
        let max_y = draw.height - body.height();
        self.detail_scroll = self.detail_scroll.clamp(0.0, max_y.max(0.0));
        // The changed-file rows are what opens a file's diff, by pointer and
        // by keyboard alike.
        for (index, rect) in draw.file_rows.iter().enumerate() {
            if rect.y1 < body.y0 || rect.y0 > body.y1 {
                continue;
            }
            if self.file_selection.is_none() && index == 0 {
                self.file_selection = Some(0);
            }
            self.semantic.push(Target {
                rect: *rect,
                hit: Hit::CommitFile(index),
            });
            if rect.contains(self.pointer.unwrap_or(Point::new(f64::MIN, f64::MIN))) {
                self.targets.push(Target {
                    rect: *rect,
                    hit: Hit::CommitFile(index),
                });
            }
        }
        self.scroll_regions.push(ScrollRegion {
            rect: body,
            scroller: Scroller::Detail,
            max_y,
            max_x: 0.0,
            ground: ctx.theme.tokens.colors.surfaces.content,
        });
    }

    /// The commit's changed files, read from the repository the first time
    /// the commit is opened. A repository that cannot answer leaves the
    /// group out rather than showing an empty one that means "no changes".
    pub(super) fn load_commit_files(&mut self, commit: &str) {
        if self.commit_files.contains_key(commit) {
            return;
        }
        let Some(path) = self.repo() else { return };
        match crate::repo::commit_files(&path, commit) {
            Ok(files) => {
                self.commit_files.insert(commit.to_owned(), files);
            }
            Err(error) => {
                self.last_error = Some(error.to_string());
            }
        }
    }

    /// Whatever a diff view drew is reachable where it was drawn: a control
    /// painted here and recorded nowhere is one that ignores a press and that
    /// a reader never hears.
    fn record_drawn(&mut self, targets: &[(Rect, DiffHit)]) {
        for &(rect, hit) in targets {
            if rect.width() > 0.0 && rect.height() > 0.0 {
                self.targets.push(Target {
                    rect,
                    hit: Hit::Diff(hit),
                });
                self.semantic.push(Target {
                    rect,
                    hit: Hit::Diff(hit),
                });
            }
        }
    }

    /// One file's changes in a commit: the same diff view the working tree
    /// uses, with the commit and file as its heading and no stage or discard
    /// actions (a commit is history; it is read, not edited).
    pub(super) fn paint_commit_diff(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        body: Rect,
        commit_id: &str,
        file: &str,
    ) {
        let key = (commit_id.to_owned(), file.to_owned());
        if !self.commit_diffs.contains_key(&key) {
            let Some(path) = self.repo() else { return };
            match crate::repo::commit_file_diff(&path, commit_id, file) {
                Ok(diff) => {
                    self.commit_diffs.insert(key.clone(), diff);
                }
                Err(error) => {
                    self.last_error = Some(error.to_string());
                    self.commit_diff = None;
                    return;
                }
            }
        }
        let Some(diff) = self.commit_diffs.get(&key) else {
            return;
        };
        // Heading: which commit, which file, and the way back.
        let theme = ctx.theme;
        let s = &theme.tokens.spacing;
        let heading = t_args(
            "git-commit-file-heading",
            &[
                ("commit", history::short_id(commit_id).into()),
                ("file", file.into()),
            ],
        );
        let back = t("git-back-to-commit");
        let style = theme.text(TextRole::Body);
        let band_h =
            f64::from(ctx.text.measure_style("Ag", None, &style).1) + 2.0 * f64::from(s.s2);
        let band = Rect::new(body.x0, body.y0, body.x1, body.y0 + band_h);
        ctx.text(
            &heading,
            &style,
            theme.text_primary,
            Point::new(band.x0 + f64::from(s.s3), band.y0 + f64::from(s.s2)),
            None,
        );
        let back_face = ButtonFace::new(
            Rect::new(
                band.x1 - f64::from(s.s3) - 120.0,
                band.y0,
                band.x1 - f64::from(s.s3),
                band.y1,
            ),
            &back,
        );
        let back_w = back_face.intrinsic_width(ctx.text, theme);
        let back_rect = Rect::new(
            band.x1 - f64::from(s.s3) - back_w,
            band.y0,
            band.x1 - f64::from(s.s3),
            band.y1,
        );
        ui::widget::Button::paint(
            ctx.scene,
            ctx.transform,
            ctx.text,
            theme,
            &back_face.hovered(self.pointer.is_some_and(|p| back_rect.contains(p))),
        );
        self.semantic.push(Target {
            rect: back_rect,
            hit: Hit::BackToCommit,
        });
        self.targets.push(Target {
            rect: back_rect,
            hit: Hit::BackToCommit,
        });
        let diff_area = Rect::new(body.x0, band.y1, body.x1, body.y1);
        let out = diffview::paint(
            ctx,
            diff_area,
            &diffview::DiffInput {
                diff,
                scroll: self.diff_scroll,
                hscroll: self.diff_hscroll,
                settings: self.settings,
                staged: false,
                can_discard: false,
                // The change is already committed: there is nothing here to
                // stage, and the working tree's selection is not this file.
                actions: false,
                pointer: self.pointer,
            },
        );
        self.record_drawn(&out.targets);
        let max_y = out.content_height - out.viewport.height();
        self.diff_scroll = self.diff_scroll.clamp(0.0, max_y.max(0.0));
        self.diff_hscroll = self.diff_hscroll.clamp(0.0, out.max_hscroll);
        self.scroll_regions.push(ScrollRegion {
            rect: out.viewport,
            scroller: Scroller::Diff,
            max_y,
            max_x: out.max_hscroll,
            ground: ctx.theme.tokens.colors.surfaces.content,
        });
    }
}
