//! The branches screen: local and remote branches, tags, stashes and
//! remotes, each a section of the page with its rows.
//!
//! Sections stand on the window ground, separated by space; on a wide window
//! they flow into two columns. Every row is a toolkit [`ListRowFace`]; what
//! can be done to a row (check out, delete, apply) is its hover action. The
//! remote commands (fetch, pull, push) live in the window's header row.

use vello::kurbo::{Point, Rect};

use ui::icons::Icon;
use ui::paint::PaintCtx;
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::widget::{
    BadgeFace, Button, ButtonFace, ButtonRole, ControlSize, Interaction, ListRowFace, RowAction,
};

use crate::history::short_id;
use crate::i18n::{t, t_args};
use crate::model::{Branch, RepoModel, Stash};

/// A remote command from the header row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemoteAction {
    Fetch,
    Pull,
    Push,
}

impl RemoteAction {
    pub const ALL: [Self; 3] = [Self::Fetch, Self::Pull, Self::Push];

    pub fn label(self) -> String {
        t(match self {
            Self::Fetch => "git-fetch",
            Self::Pull => "git-pull",
            Self::Push => "git-push",
        })
    }

    /// The command's icon, for a header too narrow for its label.
    pub fn icon(self) -> Icon {
        match self {
            Self::Fetch => Icon::CloudDownload,
            Self::Pull => Icon::ArrowDown,
            Self::Push => Icon::ArrowUp,
        }
    }
}

/// Something done to one branch or stash.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BranchAction {
    /// Check out the named local branch (or remote-tracking ref).
    Checkout(String),
    /// Delete the named local branch.
    Delete(String),
    /// Apply the stash at the given index.
    StashApply(usize),
}

/// What a press on the screen asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PanelHit {
    Branch(BranchAction),
    /// The new branch name field.
    BranchName,
    /// The "Create branch" button.
    CreateBranch,
}

/// What the screen shows this frame.
pub struct PanelsInput<'a> {
    pub model: &'a RepoModel,
    pub scroll: f64,
    pub pointer: Option<Point>,
    /// A repository on disk: branches can be created here.
    pub can_create: bool,
    /// The new branch name typed so far.
    pub branch_name: &'a str,
}

/// What one paint produced.
pub struct PanelsDraw {
    /// Targets under the pointer this frame (rows' hover actions included).
    pub targets: Vec<(Rect, PanelHit)>,
    /// Every row action, hovered or not, at its row: what assistive
    /// technology and agents can reach.
    pub semantic: Vec<(Rect, PanelHit)>,
    /// Where the owner lays out the new branch name field.
    pub branch_field: Rect,
    /// Height of everything, for clamping the scroll.
    pub content_height: f64,
}

/// One section of the page.
struct SectionRows<'a> {
    title: String,
    rows: Vec<RowSpec<'a>>,
    /// Holds the new branch form.
    form: bool,
    /// Stands in the leading column of a two-column page.
    leading: bool,
}

/// One row: what it shows and what can be done to it.
struct RowSpec<'a> {
    title: &'a str,
    secondary: Option<String>,
    meta: Option<String>,
    current: bool,
    actions: Vec<(Icon, String, BranchAction)>,
}

/// How a branch stands against its upstream, in words.
pub fn tracking(branch: &Branch) -> String {
    tracking_words(branch, false)
}

/// How the checked-out branch stands against its upstream: the same words,
/// saying it is the one checked out.
pub fn current_tracking(branch: &Branch) -> String {
    tracking_words(branch, true)
}

fn tracking_words(branch: &Branch, current: bool) -> String {
    let current = ("current", if current { "yes" } else { "no" }.into());
    let Some(upstream) = &branch.upstream else {
        return t_args("git-tracking-none", &[current]);
    };
    let upstream = ("upstream", upstream.as_str().into());
    let ahead = ("ahead", branch.ahead.into());
    let behind = ("behind", branch.behind.into());
    match (branch.ahead, branch.behind) {
        (0, 0) => t_args("git-tracking-even", &[current, upstream]),
        (_, 0) => t_args("git-tracking-ahead", &[current, upstream, ahead]),
        (0, _) => t_args("git-tracking-behind", &[current, upstream, behind]),
        _ => t_args("git-tracking-diverged", &[current, upstream, ahead, behind]),
    }
}

/// A stash's message without git's `On <branch>:` prefix.
pub fn stash_title(stash: &Stash) -> &str {
    let message = stash.message.as_str();
    for prefix in ["WIP on ", "On "] {
        if let Some(rest) = message.strip_prefix(prefix)
            && let Some((_, title)) = rest.split_once(": ")
        {
            return title;
        }
    }
    message
}

fn checkout(name: &str) -> (Icon, String, BranchAction) {
    (
        Icon::Check,
        t("git-check-out"),
        BranchAction::Checkout(name.to_string()),
    )
}

fn delete(name: &str) -> (Icon, String, BranchAction) {
    (
        Icon::Trash,
        t("git-delete-branch"),
        BranchAction::Delete(name.to_string()),
    )
}

fn apply(index: usize) -> (Icon, String, BranchAction) {
    (
        Icon::CornerDownLeft,
        t("git-apply-stash"),
        BranchAction::StashApply(index),
    )
}

/// The rows of the local branches (`remote` false) or the remote ones.
fn branch_rows(model: &RepoModel, remote: bool) -> Vec<RowSpec<'_>> {
    model
        .branches
        .iter()
        .filter(|b| b.is_remote == remote)
        .map(|b| RowSpec {
            title: &b.name,
            secondary: (!remote).then(|| {
                if b.is_head {
                    current_tracking(b)
                } else {
                    tracking(b)
                }
            }),
            meta: Some(short_id(&b.tip).to_string()),
            current: b.is_head,
            actions: if b.is_head {
                Vec::new()
            } else if remote {
                vec![checkout(&b.name)]
            } else {
                vec![checkout(&b.name), delete(&b.name)]
            },
        })
        .collect()
}

fn sections(model: &RepoModel, can_create: bool) -> Vec<SectionRows<'_>> {
    let local = branch_rows(model, false);
    let remote = branch_rows(model, true);
    let tags = model
        .tags
        .iter()
        .map(|t| RowSpec {
            title: &t.name,
            secondary: t.message.clone(),
            meta: Some(short_id(&t.target).to_string()),
            current: false,
            actions: Vec::new(),
        })
        .collect();
    let stashes = model
        .stashes
        .iter()
        .map(|s| RowSpec {
            title: stash_title(s),
            secondary: Some(s.branch.clone()),
            meta: None,
            current: false,
            actions: vec![apply(s.index)],
        })
        .collect();
    let remotes = model
        .remotes
        .iter()
        .map(|r| RowSpec {
            title: &r.name,
            secondary: Some(r.url.clone()),
            meta: None,
            current: false,
            actions: Vec::new(),
        })
        .collect();
    vec![
        SectionRows {
            title: t("git-section-branches"),
            rows: local,
            form: can_create,
            leading: true,
        },
        SectionRows {
            title: t("git-section-remote-branches"),
            rows: remote,
            form: false,
            leading: true,
        },
        SectionRows {
            title: t("git-section-tags"),
            rows: tags,
            form: false,
            leading: false,
        },
        SectionRows {
            title: t("git-section-stashes"),
            rows: stashes,
            form: false,
            leading: false,
        },
        SectionRows {
            title: t("git-section-remotes"),
            rows: remotes,
            form: false,
            leading: false,
        },
    ]
}

/// Paint the screen into `area` (the content column under the header).
pub fn paint(ctx: &mut PaintCtx<'_>, area: Rect, input: &PanelsInput<'_>) -> PanelsDraw {
    let theme = ctx.theme;
    let s = &theme.tokens.spacing;
    let gutter = f64::from(s.s6);
    let column_gap = f64::from(s.s8);
    let lane = ui::content_layout::FocusedContentLane::new()
        .gutter(gutter)
        .layout(area);
    let two_columns =
        lane.width() >= 2.0 * f64::from(theme.tokens.metrics.detail_min_width) + column_gap;
    let all = sections(input.model, input.can_create);
    let mut out = PanelsDraw {
        targets: Vec::new(),
        semantic: Vec::new(),
        branch_field: Rect::ZERO,
        content_height: 0.0,
    };
    ctx.scene.push_layer(
        vello::peniko::Fill::NonZero,
        vello::peniko::BlendMode::default(),
        1.0,
        ctx.transform,
        &area,
    );
    let top = area.y0 + gutter - input.scroll;
    let columns: Vec<(Rect, Vec<&SectionRows<'_>>)> = if two_columns {
        let w = (lane.width() - column_gap) / 2.0;
        let (left, right): (Vec<_>, Vec<_>) = all
            .iter()
            .filter(|section| !section.rows.is_empty() || section.form)
            .partition(|section| section.leading);
        vec![
            (Rect::new(lane.x0, top, lane.x0 + w, area.y1), left),
            (Rect::new(lane.x1 - w, top, lane.x1, area.y1), right),
        ]
    } else {
        vec![(
            Rect::new(lane.x0, top, lane.x1, area.y1),
            all.iter()
                .filter(|section| !section.rows.is_empty() || section.form)
                .collect(),
        )]
    };
    let mut bottom = top;
    for (column, list) in columns {
        let mut y = column.y0;
        for (i, section) in list.into_iter().enumerate() {
            if i > 0 {
                y += f64::from(s.s8);
            }
            y = paint_section(ctx, column, area, section, input, y, &mut out);
        }
        bottom = bottom.max(y);
    }
    ctx.scene.pop_layer();
    out.content_height = bottom - top + 2.0 * gutter;
    out
}

/// One section from `y`: its heading with the count, the new branch form
/// when it has one, and its rows. Returns the `y` under it.
fn paint_section(
    ctx: &mut PaintCtx<'_>,
    column: Rect,
    clip: Rect,
    section: &SectionRows<'_>,
    input: &PanelsInput<'_>,
    y: f64,
    out: &mut PanelsDraw,
) -> f64 {
    let theme = ctx.theme;
    let s = &theme.tokens.spacing;
    let heading = theme.text(TextRole::Heading);
    let line = f64::from(ctx.text.measure_style("Ag", None, &heading).1);
    let band = Rect::new(
        column.x0,
        y,
        column.x1,
        y + line.max(BadgeFace::height(theme)),
    );
    let title_w = f64::from(ctx.text.measure_style(&section.title, None, &heading).0);
    ctx.text_role_in_band(
        &section.title,
        TextRole::Heading,
        theme.text_primary,
        band.x0,
        band,
        None,
    );
    let count = section.rows.len().to_string();
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
    let mut y = band.y1 + f64::from(s.s2);

    if section.form {
        y = paint_branch_form(ctx, column, input, y, out) + f64::from(s.s2);
    }

    // Rows run out to the section's edges by their own padding, so their
    // text lines up with the heading.
    let pad = f64::from(s.s3);
    let pointer = input.pointer.filter(|p| clip.contains(*p));
    for (i, row) in section.rows.iter().enumerate() {
        let actions: Vec<RowAction<'_>> = row
            .actions
            .iter()
            .map(|(icon, label, _)| RowAction { icon: *icon, label })
            .collect();
        let mut face = ListRowFace::new(row.title).actions(&actions);
        if let Some(secondary) = row.secondary.as_deref() {
            face = face.secondary(secondary);
        }
        if let Some(meta) = row.meta.as_deref() {
            face = face.meta(meta);
        }
        // The checked-out branch is named in its secondary line and set in
        // strong type; its trailing edge stays where every other row's is.
        if row.current {
            face = face.emphasized(true);
        }
        if i + 1 < section.rows.len() {
            face = face.separator(false);
        }
        let h = face.height(ctx.text, theme);
        let rect = Rect::new(column.x0 - pad, y, column.x1 + pad, y + h);
        y += h;
        if rect.y1 < clip.y0 || rect.y0 > clip.y1 {
            continue;
        }
        let hovered = pointer.is_some_and(|p| rect.contains(p));
        let interaction = Interaction {
            hovered,
            ..Interaction::default()
        };
        let action_rects = face
            .clone()
            .interaction(interaction)
            .action_rects(rect, theme);
        let hovered_action = pointer.and_then(|p| action_rects.iter().position(|r| r.contains(p)));
        face.interaction(interaction)
            .hovered_action(hovered_action)
            .paint(ctx, rect);
        for ((_, _, action), r) in row.actions.iter().zip(&action_rects) {
            out.targets
                .push((r.intersect(clip), PanelHit::Branch(action.clone())));
        }
        for (_, _, action) in &row.actions {
            out.semantic
                .push((rect.intersect(clip), PanelHit::Branch(action.clone())));
        }
    }
    y
}

/// The new branch form: the name field (hosted by the owner) and its
/// button. Returns the `y` under it.
fn paint_branch_form(
    ctx: &mut PaintCtx<'_>,
    column: Rect,
    input: &PanelsInput<'_>,
    y: f64,
    out: &mut PanelsDraw,
) -> f64 {
    let theme = ctx.theme;
    let h = ButtonFace::height(ControlSize::Regular, theme);
    let label = t("git-create-branch");
    let face = ButtonFace::new(Rect::ZERO, &label);
    let w = face.intrinsic_width(ctx.text, theme);
    let button = Rect::new(column.x1 - w, y, column.x1, y + h);
    let field = Rect::new(
        column.x0,
        y,
        button.x0 - f64::from(theme.tokens.spacing.s2),
        y + h,
    );
    let enabled = !input.branch_name.trim().is_empty();
    let face = ButtonFace::new(button, &label)
        .role(ButtonRole::Secondary)
        .disabled(!enabled);
    let hovered = enabled && input.pointer.is_some_and(|p| face.contains(theme, p));
    let face = face.hovered(hovered);
    Button::paint(ctx.scene, ctx.transform, ctx.text, theme, &face);
    out.branch_field = field;
    out.targets.push((field, PanelHit::BranchName));
    out.semantic.push((field, PanelHit::BranchName));
    if enabled {
        out.targets.push((button, PanelHit::CreateBranch));
    }
    out.semantic.push((button, PanelHit::CreateBranch));
    button.y1
}

/// The header's remote commands, laid out from `right` leftwards in `band`:
/// labelled where they fit right of `min_x`, icons where only those do.
pub fn remote_buttons(
    ctx: &mut PaintCtx<'_>,
    band: Rect,
    right: f64,
    min_x: f64,
    pointer: Option<Point>,
) -> Vec<(Rect, RemoteAction)> {
    let theme = ctx.theme;
    let gap = f64::from(theme.tokens.spacing.s2);
    let h = ButtonFace::height(ControlSize::Regular, theme);
    let y = band.center().y - h / 2.0;
    let labelled = right - remote_buttons_width(ctx.text, theme) >= min_x;
    let mut x1 = right;
    let mut placed = Vec::new();
    for action in RemoteAction::ALL.iter().rev() {
        let label = action.label();
        let w = if labelled {
            ButtonFace::new(Rect::ZERO, &label).intrinsic_width(ctx.text, theme)
        } else {
            h
        };
        let rect = Rect::new(x1 - w, y, x1, y + h);
        if rect.x0 < min_x {
            break;
        }
        let face = if labelled {
            ButtonFace::new(rect, &label)
        } else {
            ButtonFace::icon_only(rect, action.icon(), &label)
        };
        let hovered = pointer.is_some_and(|p| face.contains(theme, p));
        let face = face.hovered(hovered);
        Button::paint(ctx.scene, ctx.transform, ctx.text, theme, &face);
        placed.push((rect, *action));
        x1 = rect.x0 - gap;
    }
    placed.reverse();
    placed
}

/// Width the remote commands need, gaps included.
pub fn remote_buttons_width(text: &mut ui::text_render::TextRenderer, theme: &Theme) -> f64 {
    let gap = f64::from(theme.tokens.spacing.s2);
    RemoteAction::ALL
        .iter()
        .map(|a| ButtonFace::new(Rect::ZERO, &a.label()).intrinsic_width(text, theme))
        .sum::<f64>()
        + gap * (RemoteAction::ALL.len() - 1) as f64
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::text_render::TextRenderer;
    use vello::Scene;
    use vello::kurbo::Affine;

    fn draw(width: f64) -> (PanelsDraw, RepoModel) {
        let (model, _) = crate::sample::model();
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let mut scene = Scene::new();
        let mut ctx = PaintCtx::new(&mut scene, Affine::IDENTITY, &mut text, &theme);
        let out = paint(
            &mut ctx,
            Rect::new(0.0, 0.0, width, 4000.0),
            &PanelsInput {
                model: &model,
                scroll: 0.0,
                pointer: None,
                can_create: true,
                branch_name: "",
            },
        );
        (out, model)
    }

    /// Every branch other than the checked-out one can be checked out and
    /// deleted, every stash applied; the actions are reachable without a
    /// pointer, and sections stack in one column on a narrow window.
    #[test]
    fn branch_and_stash_actions_are_reachable() {
        for width in [1280.0, 480.0] {
            let (out, model) = draw(width);
            for branch in model.branches.iter().filter(|b| !b.is_head) {
                assert!(out.semantic.iter().any(|(_, hit)| *hit
                    == PanelHit::Branch(BranchAction::Checkout(branch.name.clone()))));
            }
            for branch in model.branches.iter().filter(|b| !b.is_head && !b.is_remote) {
                assert!(
                    out.semantic.iter().any(|(_, hit)| *hit
                        == PanelHit::Branch(BranchAction::Delete(branch.name.clone())))
                );
            }
            for stash in &model.stashes {
                assert!(out.semantic.iter().any(
                    |(_, hit)| *hit == PanelHit::Branch(BranchAction::StashApply(stash.index))
                ));
            }
            assert!(out.branch_field.width() > 0.0);
            assert!(out.content_height > 0.0);
        }
        let (wide, _) = draw(1280.0);
        let (narrow, _) = draw(480.0);
        assert!(narrow.content_height > wide.content_height);
    }

    #[test]
    fn tracking_and_stash_titles_read_as_sentences() {
        let mut branch = Branch {
            name: "main".into(),
            is_head: true,
            is_remote: false,
            upstream: Some("origin/main".into()),
            ahead: 2,
            behind: 0,
            tip: "a1b2c3d".into(),
        };
        assert_eq!(tracking(&branch), "2 ahead of origin/main");
        branch.behind = 1;
        assert_eq!(tracking(&branch), "2 ahead, 1 behind origin/main");
        branch.upstream = None;
        assert_eq!(tracking(&branch), "No upstream branch");
        let stash = Stash {
            index: 0,
            message: "On main: try the msaa path".into(),
            branch: "main".into(),
        };
        assert_eq!(stash_title(&stash), "try the msaa path");
    }
}
