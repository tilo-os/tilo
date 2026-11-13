//! The changes column: what is staged, what is not yet, and the commit box. Each
//! area is a section (a title, a count and one action for the whole area) over
//! toolkit `ListRowFace` rows whose leading badge says how the file changed, whose
//! secondary line names its folder and whose meta counts the lines added and
//! removed. Every row has the height of a two-line row, so a file at the repository
//! root keeps the list's rhythm; staging a single file is the row's hover action.
//! The commit box is pinned under the list.

use vello::kurbo::{Point, Rect};

use ui::paint::PaintCtx;
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::widget::{
    BadgeFace, Button, ButtonFace, ButtonRole, ControlSize, EmptyState, EmptyStateContent,
    Interaction, ListRowFace, RowAction, RowLeading, badge_column_width,
};

use crate::i18n::{t, t_args};
use crate::model::{ChangeArea, ChangeKind, FileChange, RepoModel};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangesHit {
    SelectFile(ChangeArea, usize),
    ToggleStage(ChangeArea, usize),
    StageAll(ChangeArea),
    UnstageAll(ChangeArea),
    Commit,
    FocusMessage,
}

pub struct ChangesInput<'a> {
    pub model: &'a RepoModel,
    pub selected: Option<(ChangeArea, usize)>,
    pub scroll: f64,
    /// The commit message typed so far: the commit button is enabled once it and the staged area
    /// are both non-empty.
    pub message: &'a str,
    pub pointer: Option<Point>,
}

/// What one paint produced: every target, where the owner lays out the message field, and the
/// list's scroll extent.
pub struct ChangesDraw {
    pub targets: Vec<(Rect, ChangesHit)>,
    pub commit_input: Rect,
    /// The commit button, enabled or not: assistive technology reaches it either way, though only
    /// an enabled button is a press target.
    pub commit_button: Rect,
    pub commit_enabled: bool,
    pub content_height: f64,
    pub viewport_height: f64,
}

pub fn section_title(area: ChangeArea) -> String {
    t(match area {
        ChangeArea::Staged => "git-section-staged",
        ChangeArea::Unstaged => "git-section-unstaged",
        ChangeArea::Untracked => "git-section-untracked",
    })
}

fn section_action(area: ChangeArea) -> String {
    t(match area {
        ChangeArea::Staged => "git-unstage-all",
        ChangeArea::Unstaged | ChangeArea::Untracked => "git-stage-all",
    })
}

/// The commit button's label: what happens, on which branch.
pub fn commit_label(model: &RepoModel) -> String {
    if model.head.is_empty() {
        t("git-commit")
    } else {
        t_args("git-commit-to", &[("branch", model.head.as_str().into())])
    }
}

/// `+12 −3`: the lines a change adds and removes, or `None` when it has no line counts (a binary
/// file).
pub fn line_counts(additions: u32, deletions: u32) -> Option<String> {
    match (additions, deletions) {
        (0, 0) => None,
        (a, 0) => Some(format!("+{a}")),
        (0, d) => Some(format!("\u{2212}{d}")),
        (a, d) => Some(format!("+{a} \u{2212}{d}")),
    }
}

struct Metrics {
    inset: f64,
    pad: f64,
    section_gap: f64,
    header_h: f64,
    header_gap: f64,
    control_h: f64,
    control_gap: f64,
}

impl Metrics {
    fn of(theme: &Theme) -> Self {
        let s = &theme.tokens.spacing;
        Self {
            inset: f64::from(s.s2),
            pad: f64::from(s.s3),
            section_gap: f64::from(s.s4),
            header_h: ButtonFace::height(ControlSize::Small, theme),
            header_gap: f64::from(s.s1),
            control_h: ButtonFace::height(ControlSize::Regular, theme),
            control_gap: f64::from(s.s2),
        }
    }

    fn commit_box_height(&self) -> f64 {
        self.pad * 2.0 + self.control_h * 2.0 + self.control_gap
    }
}

pub fn paint(ctx: &mut PaintCtx<'_>, area: Rect, input: &ChangesInput<'_>) -> ChangesDraw {
    let m = Metrics::of(ctx.theme);
    let box_top = (area.y1 - m.commit_box_height()).max(area.y0);
    let list = Rect::new(area.x0, area.y0, area.x1, box_top);
    let mut targets = Vec::new();

    let model = input.model;
    let sections = [
        (ChangeArea::Staged, model.staged.as_slice()),
        (ChangeArea::Unstaged, model.unstaged.as_slice()),
        (ChangeArea::Untracked, model.untracked.as_slice()),
    ];
    let content_height = if model.change_count() == 0 {
        paint_clean(ctx, list);
        list.height()
    } else {
        ctx.scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            ctx.transform,
            &list,
        );
        let badges: Vec<&str> = model
            .all_changes()
            .iter()
            .map(|change| change.kind.badge())
            .collect();
        let badge_w = badge_column_width(ctx.text, ctx.theme, &badges);
        let row_h = ListRowFace::new("")
            .secondary("")
            .height(ctx.text, ctx.theme);
        let mut pass = ListPass {
            list,
            m: &m,
            input,
            badge_w,
            row_h,
            targets: &mut targets,
        };
        let top = list.y0 + m.inset - input.scroll;
        let mut y = top;
        let mut first = true;
        for (change_area, files) in sections {
            if files.is_empty() {
                continue;
            }
            if !first {
                y += m.section_gap;
            }
            first = false;
            y = pass.section(ctx, change_area, files, y);
        }
        ctx.scene.pop_layer();
        y + m.inset - top
    };

    let commit = paint_commit_box(
        ctx,
        Rect::new(area.x0, box_top, area.x1, area.y1),
        &m,
        input,
        &mut targets,
    );

    ChangesDraw {
        targets,
        commit_input: commit.field,
        commit_button: commit.button,
        commit_enabled: commit.enabled,
        content_height,
        viewport_height: list.height(),
    }
}

fn paint_clean(ctx: &mut PaintCtx<'_>, list: Rect) {
    let title = t("git-no-changes");
    let content = EmptyStateContent::new(&title);
    EmptyState::paint(ctx, list, &content, Point::new(f64::MIN, f64::MIN));
}

struct ListPass<'a, 'b> {
    list: Rect,
    m: &'a Metrics,
    input: &'a ChangesInput<'b>,
    /// The leading badge column every row shares, so names line up.
    badge_w: f64,
    row_h: f64,
    targets: &'a mut Vec<(Rect, ChangesHit)>,
}

impl ListPass<'_, '_> {
    fn section(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        change_area: ChangeArea,
        files: &[FileChange],
        y: f64,
    ) -> f64 {
        let header = Rect::new(
            self.list.x0 + self.m.inset,
            y,
            self.list.x1 - self.m.inset,
            y + self.m.header_h,
        );
        if header.y1 > self.list.y0 && header.y0 < self.list.y1 {
            self.section_header(ctx, header, change_area, files.len());
        }
        let mut y = header.y1 + self.m.header_gap;
        for (index, file) in files.iter().enumerate() {
            let next_selected = self.input.selected == Some((change_area, index + 1));
            y = self.row(
                ctx,
                (change_area, index),
                file,
                y,
                next_selected,
                index + 1 < files.len(),
            );
        }
        y
    }

    fn section_header(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        band: Rect,
        change_area: ChangeArea,
        count: usize,
    ) {
        let theme = ctx.theme;
        let pad = ui::chrome::row_bleed(theme);
        let gap = f64::from(theme.tokens.spacing.s2);

        // The action trails, its label ending on the rows' text edge where their line counts end;
        // the title takes what is left.
        let label = section_action(change_area);
        let probe = ButtonFace::new(Rect::ZERO, &label)
            .role(ButtonRole::Plain)
            .control_size(ControlSize::Small);
        let action_w = probe.intrinsic_width(ctx.text, theme);
        let label_w = f64::from(
            ctx.text
                .measure_style(&label, None, &ControlSize::Small.label_style(theme))
                .0,
        );
        let label_inset = ((action_w - label_w) / 2.0).max(0.0);
        let x1 = band.x1 - pad + label_inset;
        let action = Rect::new(x1 - action_w, band.y0, x1, band.y1);
        let face = ButtonFace::new(action, &label)
            .role(ButtonRole::Plain)
            .control_size(ControlSize::Small);
        let hovered = self.input.pointer.is_some_and(|p| face.contains(theme, p));
        let face = face.hovered(hovered);
        Button::paint(ctx.scene, ctx.transform, ctx.text, theme, &face);
        self.push(
            face.hit_rect(theme),
            if change_area == ChangeArea::Staged {
                ChangesHit::UnstageAll(change_area)
            } else {
                ChangesHit::StageAll(change_area)
            },
        );

        let count = count.to_string();
        let count_w = BadgeFace::intrinsic_width(&count, ctx.text, theme);
        let title_x = band.x0 + pad;
        let title_room = (action.x0 - gap - count_w - gap - title_x).max(0.0);
        let style = theme.text(TextRole::Subheading);
        let title = ctx.text.elide_to_width_style(
            &section_title(change_area),
            ui::num::f32_of_f64(title_room),
            &style,
        );
        let title_w = f64::from(ctx.text.measure_style(&title, None, &style).0);
        ctx.text_role_in_band(
            &title,
            TextRole::Subheading,
            theme.text_dim,
            title_x,
            band,
            None,
        );
        let badge_h = BadgeFace::height(theme);
        let badge = Rect::from_origin_size(
            Point::new(title_x + title_w + gap, band.center().y - badge_h / 2.0),
            (count_w, badge_h),
        );
        BadgeFace::new(badge, &count).paint(ctx.scene, ctx.transform, ctx.text, theme);
    }

    fn row(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        key: (ChangeArea, usize),
        file: &FileChange,
        y: f64,
        next_selected: bool,
        has_next: bool,
    ) -> f64 {
        let (change_area, index) = key;
        let staged = change_area == ChangeArea::Staged;
        let action_label = t(if staged { "git-unstage" } else { "git-stage" });
        let actions = [RowAction {
            icon: if staged {
                ui::icons::Icon::Minus
            } else {
                ui::icons::Icon::Plus
            },
            label: &action_label,
        }];
        let counts = line_counts(file.additions, file.deletions);
        let title = row_title(file);
        let dir = file.dir();
        let mut face = ListRowFace::new(&title)
            .leading(RowLeading::Badge(file.kind.badge()))
            .leading_column(self.badge_w)
            .selected(self.input.selected == Some(key))
            .actions(&actions);
        if !dir.is_empty() {
            face = face.secondary(dir);
        }
        let adds = format!("+{}", file.additions);
        let dels = format!("\u{2212}{}", file.deletions);
        let spans: Vec<(&str, ui::widget::Tone)> = [
            (file.additions > 0).then_some((adds.as_str(), ui::widget::Tone::Success)),
            (file.deletions > 0).then_some((dels.as_str(), ui::widget::Tone::Danger)),
        ]
        .into_iter()
        .flatten()
        .collect();
        if counts.is_some() {
            face = face.meta_spans(&spans);
        }
        let h = self.row_h.max(face.height(ctx.text, ctx.theme));
        let rect = Rect::new(
            self.list.x0 + self.m.inset,
            y,
            self.list.x1 - self.m.inset,
            y + h,
        );
        let visible = rect.y1 > self.list.y0 && rect.y0 < self.list.y1;
        if !visible {
            return rect.y1;
        }
        let pointer = self.input.pointer.filter(|p| self.list.contains(*p));
        let hovered = pointer.is_some_and(|p| rect.contains(p));
        let action_rects = face
            .clone()
            .interaction(Interaction {
                hovered,
                ..Interaction::default()
            })
            .action_rects(rect, ctx.theme);
        let hovered_action = pointer.and_then(|p| action_rects.iter().position(|r| r.contains(p)));
        let mut face = face
            .interaction(Interaction {
                hovered,
                ..Interaction::default()
            })
            .hovered_action(hovered_action);
        if has_next {
            face = face.separator(next_selected);
        }
        face.paint(ctx, rect);
        // Row actions first: they sit on top of the row they belong to.
        for action in action_rects {
            self.push(action, ChangesHit::ToggleStage(change_area, index));
        }
        self.push(rect, ChangesHit::SelectFile(change_area, index));
        rect.y1
    }

    /// Record a target, clipped to the list viewport so a row scrolled under the commit box never
    /// takes its presses.
    fn push(&mut self, rect: Rect, hit: ChangesHit) {
        let clipped = rect.intersect(self.list);
        if clipped.width() > 0.0 && clipped.height() > 0.0 {
            self.targets.push((clipped, hit));
        }
    }
}

fn row_title(file: &FileChange) -> String {
    match &file.old_path {
        Some(old) if file.kind == ChangeKind::Renamed => {
            let old_name = old.rsplit('/').next().unwrap_or(old);
            if old_name == file.name() {
                file.name().to_string()
            } else {
                format!("{old_name} \u{2192} {}", file.name())
            }
        }
        _ => file.name().to_string(),
    }
}

struct CommitBox {
    field: Rect,
    button: Rect,
    enabled: bool,
}

fn paint_commit_box(
    ctx: &mut PaintCtx<'_>,
    rect: Rect,
    m: &Metrics,
    input: &ChangesInput<'_>,
    targets: &mut Vec<(Rect, ChangesHit)>,
) -> CommitBox {
    let theme = ctx.theme;
    ui::surface::separator(
        ctx.scene,
        ctx.transform,
        rect.x0,
        rect.x1,
        rect.y0,
        0.0,
        theme.tokens.colors.surfaces.content,
    );
    let field = Rect::new(
        rect.x0 + m.pad,
        rect.y0 + m.pad,
        rect.x1 - m.pad,
        rect.y0 + m.pad + m.control_h,
    );
    targets.push((field, ChangesHit::FocusMessage));

    let button = Rect::new(
        field.x0,
        field.y1 + m.control_gap,
        field.x1,
        field.y1 + m.control_gap + m.control_h,
    );
    let enabled = !input.model.staged.is_empty() && !input.message.trim().is_empty();
    let label = commit_label(input.model);
    let face = ButtonFace::new(button, &label)
        .role(ButtonRole::Primary)
        .disabled(!enabled);
    let hovered = enabled && input.pointer.is_some_and(|p| face.contains(theme, p));
    let face = face.hovered(hovered);
    Button::paint(ctx.scene, ctx.transform, ctx.text, theme, &face);
    if enabled {
        targets.push((button, ChangesHit::Commit));
    }
    CommitBox {
        field,
        button,
        enabled,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::text_render::TextRenderer;
    use vello::Scene;
    use vello::kurbo::Affine;

    fn draw(model: &RepoModel, width: f64, pointer: Option<Point>) -> ChangesDraw {
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let mut scene = Scene::new();
        let mut ctx = PaintCtx::new(&mut scene, Affine::IDENTITY, &mut text, &theme);
        paint(
            &mut ctx,
            Rect::new(0.0, 0.0, width, 700.0),
            &ChangesInput {
                model,
                selected: Some((ChangeArea::Staged, 0)),
                scroll: 0.0,
                message: "",
                pointer,
            },
        )
    }

    /// Every row is a target, a row's stage action exists only under the
    /// pointer, section actions are present for every non-empty area, and no
    /// target reaches under the pinned commit box.
    #[test]
    fn rows_sections_and_the_commit_box_are_separate_targets() {
        let (model, _) = crate::sample::model();
        let out = draw(&model, 340.0, None);
        let rows = out
            .targets
            .iter()
            .filter(|(_, hit)| matches!(hit, ChangesHit::SelectFile(..)))
            .count();
        assert_eq!(rows, model.change_count());
        assert!(
            !out.targets
                .iter()
                .any(|(_, hit)| matches!(hit, ChangesHit::ToggleStage(..))),
            "row actions are hover actions"
        );
        for area in [ChangeArea::Unstaged, ChangeArea::Untracked] {
            assert!(
                out.targets
                    .iter()
                    .any(|(_, h)| *h == ChangesHit::StageAll(area))
            );
        }
        assert!(
            out.targets
                .iter()
                .any(|(_, h)| *h == ChangesHit::UnstageAll(ChangeArea::Staged))
        );
        assert!(
            !out.targets.iter().any(|(_, h)| *h == ChangesHit::Commit),
            "an empty message leaves the commit button disabled"
        );
        for (rect, hit) in &out.targets {
            if !matches!(hit, ChangesHit::FocusMessage) {
                assert!(
                    rect.y1 <= out.commit_input.y0,
                    "{hit:?} at {rect:?} reaches under the commit box"
                );
            }
        }

        // Hovering the first row offers its action, on top of the row.
        let first = out
            .targets
            .iter()
            .find(|(_, hit)| matches!(hit, ChangesHit::SelectFile(ChangeArea::Staged, 0)))
            .map(|(rect, _)| *rect)
            .expect("the first staged row");
        let hovered = draw(&model, 340.0, Some(first.center()));
        let action = hovered
            .targets
            .iter()
            .position(|(_, hit)| *hit == ChangesHit::ToggleStage(ChangeArea::Staged, 0))
            .expect("the hovered row offers to unstage");
        let row = hovered
            .targets
            .iter()
            .position(|(_, hit)| *hit == ChangesHit::SelectFile(ChangeArea::Staged, 0))
            .expect("the row itself");
        assert!(action < row, "the action is hit-tested before the row");
    }

    #[test]
    fn line_counts_and_labels_read_as_written() {
        assert_eq!(line_counts(12, 3).as_deref(), Some("+12 \u{2212}3"));
        assert_eq!(line_counts(4, 0).as_deref(), Some("+4"));
        assert_eq!(line_counts(0, 41).as_deref(), Some("\u{2212}41"));
        assert_eq!(line_counts(0, 0), None);
        let (model, _) = crate::sample::model();
        assert_eq!(commit_label(&model), "Commit to main");
    }
}
