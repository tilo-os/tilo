//! The repository panels screen: branches, tags, stashes and remotes,
//! each in its own card, plus a remote-actions toolbar (fetch / pull /
//! push) that reports the current branch's ahead/behind state.
//!
//! These views are read-oriented dashboards over the model; the toolbar
//! buttons are hit-testable so the app can dispatch the corresponding
//! git operation against the live backend.

use vello::kurbo::{Point, Rect};

use ui::paint::PaintCtx;
use ui::text_render::{TextStyle, TextWeight};
use ui::widget::ButtonFace;

use crate::draw;
use crate::model::RepoModel;

/// A remote action the toolbar can request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemoteAction {
    Fetch,
    Pull,
    Push,
}

/// A branch / stash action a panel row can request. Distinct from
/// [`RemoteAction`] so the app can route them to the matching `git2` calls.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BranchAction {
    /// Check out the named local branch (or remote-tracking ref).
    Checkout(String),
    /// Delete the named local branch.
    Delete(String),
    /// Apply the stash at the given index.
    StashApply(usize),
}

/// Any clickable action emitted by the panels screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PanelHit {
    Remote(RemoteAction),
    Branch(BranchAction),
}

#[derive(Clone, Debug)]
struct HitRegion {
    rect: Rect,
    action: PanelHit,
}

pub struct PanelsView;

pub struct PanelsDraw {
    pub hits: Vec<Box<dyn Fn(Point) -> Option<PanelHit>>>,
    pub semantic_hits: Vec<(Rect, PanelHit)>,
}

impl PanelsView {
    /// Draw the panels grid into `area`. Returns hit closures for the
    /// remote-action toolbar buttons.
    pub fn draw(
        ctx: &mut PaintCtx<'_>,
        area: Rect,
        model: &RepoModel,
        hover_action: Option<RemoteAction>,
    ) -> PanelsDraw {
        ctx.fill_rect(area, ctx.theme.tokens.colors.surfaces.base);

        let mut regions: Vec<HitRegion> = Vec::new();

        // Toolbar across the top.
        let toolbar = Rect::new(area.x0, area.y0, area.x1, area.y0 + 56.0);
        Self::toolbar(ctx, toolbar, model, hover_action, &mut regions);

        // Two-column grid below the toolbar.
        let gap = 16.0;
        let body = Rect::new(
            area.x0 + gap,
            toolbar.y1 + gap,
            area.x1 - gap,
            area.y1 - gap,
        );
        let col_w = (body.width() - gap) / 2.0;
        let col1_x = body.x0;
        let col2_x = body.x0 + col_w + gap;
        let row_h = (body.height() - gap) / 2.0;

        Self::branches_card(
            ctx,
            Rect::new(col1_x, body.y0, col1_x + col_w, body.y0 + row_h),
            model,
            &mut regions,
        );
        Self::tags_card(
            ctx,
            Rect::new(col2_x, body.y0, col2_x + col_w, body.y0 + row_h),
            model,
        );
        Self::stashes_card(
            ctx,
            Rect::new(col1_x, body.y0 + row_h + gap, col1_x + col_w, body.y1),
            model,
            &mut regions,
        );
        Self::remotes_card(
            ctx,
            Rect::new(col2_x, body.y0 + row_h + gap, col2_x + col_w, body.y1),
            model,
        );

        let semantic_hits = regions
            .iter()
            .map(|region| (region.rect, region.action.clone()))
            .collect();
        let hits = regions
            .into_iter()
            .map(|r| {
                Box::new(move |p: Point| {
                    if r.rect.contains(p) {
                        Some(r.action.clone())
                    } else {
                        None
                    }
                }) as Box<dyn Fn(Point) -> Option<PanelHit>>
            })
            .collect();
        PanelsDraw {
            hits,
            semantic_hits,
        }
    }

    fn toolbar(
        ctx: &mut PaintCtx<'_>,
        rect: Rect,
        model: &RepoModel,
        hover: Option<RemoteAction>,
        regions: &mut Vec<HitRegion>,
    ) {
        let c = &ctx.theme.tokens.colors;
        ctx.fill_rect(rect, c.surfaces.raised);
        draw::hline(
            ctx,
            rect.y1,
            rect.x0,
            rect.x1,
            draw::edge(c.surfaces.raised),
        );

        // Ahead/behind for the head branch.
        let head = model.branches.iter().find(|b| b.is_head);
        let status = match head {
            Some(b) => format!(
                "{}  ↑{} ↓{}",
                b.upstream.clone().unwrap_or_else(|| "no upstream".into()),
                b.ahead,
                b.behind
            ),
            None => "detached".to_string(),
        };
        ctx.text(
            &model.head,
            &TextStyle::new(14.0),
            c.text_primary,
            Point::new(rect.x0 + 16.0, rect.y0 + 12.0),
            None,
        );
        ctx.text(
            &status,
            &TextStyle::new(11.5),
            c.text_dim,
            Point::new(rect.x0 + 16.0, rect.y0 + 32.0),
            None,
        );

        // Three action buttons on the right.
        let actions = [
            (RemoteAction::Fetch, "Fetch"),
            (RemoteAction::Pull, "Pull"),
            (RemoteAction::Push, "Push"),
        ];
        let bh = 30.0;
        let by = rect.y0 + (rect.height() - bh) / 2.0;
        let mut bx = rect.x1 - 16.0;
        for (action, label) in actions.iter().rev() {
            // Size each button from its measured label (plus symmetric
            // padding) rather than a fixed 84px that a longer locale
            // overflows; keep a comfortable minimum so short verbs stay
            // easy to hit.
            let lw = f64::from(
                ctx.text
                    .measure_weighted(label, 12.0, None, TextWeight::Medium)
                    .0,
            );
            let bw = (lw + 32.0).max(72.0);
            bx -= bw;
            let brect = Rect::new(bx, by, bx + bw, by + bh);
            let primary = *action == RemoteAction::Push;
            let hovered = hover == Some(*action);
            // The shared face paints rest / hover / primary from one palette
            // and gives the neutral verbs their rest edge (HIG §9.5).
            let face = ButtonFace::new(brect, label)
                .radius(6.0)
                .text_size(12.0)
                .hovered(hovered);
            let face = if primary { face.primary() } else { face };
            draw::button(ctx, &face);
            regions.push(HitRegion {
                rect: brect,
                action: PanelHit::Remote(*action),
            });
            bx -= 8.0;
        }
    }

    fn card_frame(ctx: &mut PaintCtx<'_>, rect: Rect, title: &str, count: usize) -> Rect {
        let c = &ctx.theme.tokens.colors;
        draw::card(ctx, rect, 10.0, c.surfaces.raised);
        // Title and count pill share the header band's centre line.
        let header_h = 40.0;
        let (cw, _) = ctx
            .text
            .measure_weighted(title, 13.0, None, TextWeight::Semibold);
        let title_y = draw::centered_top(
            ctx.text,
            title,
            13.0,
            TextWeight::Semibold,
            rect.y0,
            header_h,
        );
        ctx.text(
            title,
            &TextStyle::new(13.0).with_weight(TextWeight::Semibold),
            c.text_primary,
            Point::new(rect.x0 + 16.0, title_y),
            None,
        );
        // 12 px label → 18 px pill at 1×; the pill grows with its label.
        let pill_h = draw::pill_height(ctx.text, 12.0);
        draw::pill(
            ctx,
            Point::new(
                rect.x0 + 16.0 + f64::from(cw) + 8.0,
                rect.y0 + (header_h - pill_h) / 2.0,
            ),
            &count.to_string(),
            &draw::PillStyle {
                size: 12.0,
                fg: c.text_dim,
                bg: c.lift(0.72),
                edge: draw::PillEdge::Derived,
            },
        );
        draw::hline(
            ctx,
            rect.y0 + 40.0,
            rect.x0 + 12.0,
            rect.x1 - 12.0,
            c.lift(0.35),
        );
        // Content area below the header.
        Rect::new(rect.x0, rect.y0 + 48.0, rect.x1, rect.y1 - 12.0)
    }

    /// Clip the card content that follows to `body`; paired with `pop_layer`.
    fn clip(ctx: &mut PaintCtx<'_>, body: &Rect) {
        ctx.scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            ctx.transform,
            body,
        );
    }

    fn branches_card(
        ctx: &mut PaintCtx<'_>,
        rect: Rect,
        model: &RepoModel,
        regions: &mut Vec<HitRegion>,
    ) {
        let theme = ctx.theme;
        let c = &theme.tokens.colors;
        let body = Self::card_frame(ctx, rect, "Branches", model.branches.len());
        Self::clip(ctx, &body);
        let mut y = body.y0 + 4.0;
        for b in &model.branches {
            if y > body.y1 - 18.0 {
                break;
            }
            let row = Rect::new(body.x0, y - 2.0, body.x1, y + 20.0);
            // Icon: filled dot for head, ring for others, cloud-ish for remote.
            let icon_color = if b.is_head {
                theme.accent_primary
            } else if b.is_remote {
                c.semantic.success
            } else {
                c.text_dim
            };
            ctx.fill_round(
                Rect::new(body.x0 + 16.0, y + 6.0, body.x0 + 22.0, y + 12.0),
                3.0,
                icon_color,
            );

            let name_color = if b.is_head {
                theme.accent_primary
            } else {
                c.text_primary
            };
            let weight = if b.is_head {
                TextWeight::Semibold
            } else {
                TextWeight::Regular
            };
            ctx.text(
                &b.name,
                &TextStyle::new(12.5).with_weight(weight),
                name_color,
                Point::new(body.x0 + 30.0, y),
                Some((body.width() - 170.0) as f32),
            );

            // Right edge: tip id, ahead/behind, and a delete affordance for
            // non-head local branches.
            let mut rx = body.x1 - 16.0;
            // Where the row's own (checkout) hit rect ends: before the delete
            // affordance and its `s2` gap when one is painted, so the two
            // targets never share pixels.
            let mut checkout_x1 = rx;
            // Delete (✕) for local, non-head branches.
            if !b.is_remote && !b.is_head {
                let dw = 16.0;
                let drect = Rect::new(rx - dw, y, rx, y + 16.0);
                checkout_x1 = drect.x0 - f64::from(theme.tokens.spacing.s2);
                ctx.text(
                    "✕",
                    &TextStyle::new(12.0).with_weight(TextWeight::Medium),
                    c.semantic.danger,
                    Point::new(drect.x0 + 2.0, y),
                    None,
                );
                regions.push(HitRegion {
                    rect: drect,
                    action: PanelHit::Branch(BranchAction::Delete(b.name.clone())),
                });
                rx -= dw + 8.0;
            }
            let tip = &b.tip;
            let tw = f64::from(draw::mono_width(ctx.text, tip, 11.0));
            rx -= tw;
            draw::mono(ctx, tip, 11.0, c.neutral[5], Point::new(rx, y + 1.0));
            if b.ahead > 0 || b.behind > 0 {
                let ab = format!("↑{} ↓{}", b.ahead, b.behind);
                let abw = f64::from(ctx.text.measure(&ab, 10.5, None).0);
                rx -= abw + 10.0;
                ctx.text(
                    &ab,
                    &TextStyle::new(10.5),
                    c.text_dim,
                    Point::new(rx, y + 1.0),
                    None,
                );
            }

            // Clicking the row checks out the branch (head no-op). The delete
            // region is pushed first so it wins the hit-test for its sub-rect.
            if !b.is_head {
                regions.push(HitRegion {
                    rect: Rect::new(row.x0, row.y0, checkout_x1, row.y1),
                    action: PanelHit::Branch(BranchAction::Checkout(b.name.clone())),
                });
            }
            y += 26.0;
        }
        ctx.scene.pop_layer();
    }

    fn tags_card(ctx: &mut PaintCtx<'_>, rect: Rect, model: &RepoModel) {
        let c = &ctx.theme.tokens.colors;
        let body = Self::card_frame(ctx, rect, "Tags", model.tags.len());
        Self::clip(ctx, &body);
        // 12 px tag label → 18 px pill on a 28 px pitch at 1×; both follow the
        // measured pill at larger text scales, and the target hash / message
        // centre on the pill instead of a fixed `+ 2`.
        let pill_h = draw::pill_height(ctx.text, 12.0);
        let pitch = (pill_h + 10.0).max(28.0);
        let a = c.semantic.warning;
        let tag_pill = draw::PillStyle {
            size: 12.0,
            fg: c.surfaces.base,
            bg: ui::paint::with_alpha(a, 220),
            edge: draw::PillEdge::Bare,
        };
        let mut y = body.y0 + 4.0;
        for tag in &model.tags {
            if y > body.y1 - pill_h {
                break;
            }
            draw::pill(ctx, Point::new(body.x0 + 16.0, y), &tag.name, &tag_pill);
            let nw = draw::pill_width(ctx.text, &tag.name, 12.0) + 8.0;
            let tw = f64::from(draw::mono_width(ctx.text, &tag.target, 11.0));
            let target_y = draw::mono_centered_top(ctx.text, &tag.target, 11.0, y, pill_h);
            draw::mono(
                ctx,
                &tag.target,
                11.0,
                c.neutral[5],
                Point::new(body.x1 - 16.0 - tw, target_y),
            );
            if let Some(msg) = &tag.message {
                let msg_y = draw::centered_top(ctx.text, msg, 11.0, TextWeight::Regular, y, pill_h);
                ctx.text(
                    msg,
                    &TextStyle::new(11.0),
                    c.text_dim,
                    Point::new(body.x0 + 24.0 + nw, msg_y),
                    Some((body.width() - nw - 90.0) as f32),
                );
            }
            y += pitch;
        }
        ctx.scene.pop_layer();
    }

    fn stashes_card(
        ctx: &mut PaintCtx<'_>,
        rect: Rect,
        model: &RepoModel,
        regions: &mut Vec<HitRegion>,
    ) {
        let theme = ctx.theme;
        let c = &theme.tokens.colors;
        let body = Self::card_frame(ctx, rect, "Stashes", model.stashes.len());
        Self::clip(ctx, &body);
        let mut y = body.y0 + 4.0;
        if model.stashes.is_empty() {
            ctx.text(
                "No stashed changes",
                &TextStyle::new(12.0),
                c.neutral[5],
                Point::new(body.x0 + 16.0, y + 2.0),
                None,
            );
        }
        for s in &model.stashes {
            if y > body.y1 - 18.0 {
                break;
            }
            let idx = format!("stash@{{{}}}", s.index);
            draw::mono(
                ctx,
                &idx,
                11.0,
                theme.accent_primary,
                Point::new(body.x0 + 16.0, y + 1.0),
            );
            let iw = f64::from(draw::mono_width(ctx.text, &idx, 11.0));
            ctx.text(
                &s.message,
                &TextStyle::new(12.0),
                c.text_primary,
                Point::new(body.x0 + 24.0 + iw, y),
                Some((body.width() - iw - 90.0) as f32),
            );

            // "Apply" affordance on the right.
            let label = "Apply";
            let lw = f64::from(
                ctx.text
                    .measure_weighted(label, 11.0, None, TextWeight::Medium)
                    .0,
            );
            let lx = body.x1 - lw - 14.0;
            ctx.text(
                label,
                &TextStyle::new(11.0).with_weight(TextWeight::Medium),
                theme.accent_primary,
                Point::new(lx, y + 1.0),
                None,
            );
            regions.push(HitRegion {
                rect: Rect::new(lx - 6.0, y - 2.0, body.x1, y + 18.0),
                action: PanelHit::Branch(BranchAction::StashApply(s.index)),
            });
            y += 26.0;
        }
        ctx.scene.pop_layer();
    }

    fn remotes_card(ctx: &mut PaintCtx<'_>, rect: Rect, model: &RepoModel) {
        let c = &ctx.theme.tokens.colors;
        let body = Self::card_frame(ctx, rect, "Remotes", model.remotes.len());
        Self::clip(ctx, &body);
        let mut y = body.y0 + 4.0;
        for r in &model.remotes {
            if y > body.y1 - 28.0 {
                break;
            }
            ctx.text(
                &r.name,
                &TextStyle::new(12.5).with_weight(TextWeight::Medium),
                c.text_primary,
                Point::new(body.x0 + 16.0, y),
                None,
            );
            ctx.text(
                &r.url,
                &TextStyle::new(11.0),
                c.text_dim,
                Point::new(body.x0 + 16.0, y + 16.0),
                Some((body.width() - 32.0) as f32),
            );
            y += 40.0;
        }
        ctx.scene.pop_layer();
    }
}
