use crate::permissions::PermissionScope;
use crate::presence::{PathSegment, PresenceKey};
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::SpacingStep;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;
use taffy::style_helpers::length;
use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

/// How a [`Spacer`] takes its size.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Extent {
    /// Grow into whatever main-axis space the siblings leave (`flex_grow: 1`).
    /// The classic "push the trailing buttons to the right" spacer.
    Flexible,
    /// Exactly this many logical pixels on both axes; never grows or shrinks.
    Fixed(f32),
    /// A step on the Theme's spacing scale, resolved in `build_layout` so it
    /// follows `ui_scale` and a custom theme's spacing preference.
    Token(SpacingStep),
}

pub struct Spacer {
    rect: Rect,
    /// Set by the first `apply_layout`. A separate flag rather than a
    /// `Rect::ZERO` sentinel because a zero-extent spacer at the origin
    /// (`fixed(0.0)`, `token(SpacingStep::S0)`) is a legitimate rect.
    laid_out: bool,
    extent: Extent,
    id: Option<String>,
    presence_path: Vec<PathSegment>,
    permission_scope: Option<PermissionScope>,
}

impl Default for Spacer {
    fn default() -> Self {
        Self::new()
    }
}

impl Spacer {
    /// A flexible spacer that grows to fill the remaining main-axis space.
    pub fn new() -> Self {
        Self {
            rect: Rect::ZERO,
            laid_out: false,
            extent: Extent::Flexible,
            id: None,
            presence_path: Vec::new(),
            permission_scope: None,
        }
    }

    /// Address this spacer by a stable graph id.
    #[must_use]
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = Some(id.into());
        self
    }

    /// A fixed gap of exactly `px` logical pixels on both axes, so it reads
    /// as a vertical gap in a `Column` and a horizontal one in a `Row`
    /// without a second constructor. Replaces the
    /// `Container::new().with_no_padding().with_height(px)` idiom. Prefer
    /// [`Spacer::token`] when the value is a spacing step: this form does
    /// not scale with the theme.
    #[must_use]
    pub fn fixed(px: f32) -> Self {
        Self {
            extent: Extent::Fixed(px.max(0.0)),
            ..Self::new()
        }
    }

    /// A fixed gap of one spacing step, read from the `Theme` at layout time
    /// (`theme.tokens.spacing.step(step)`), so it is already multiplied by
    /// `ui_scale` and by a custom theme's `spacing_px`.
    #[must_use]
    pub fn token(step: SpacingStep) -> Self {
        Self {
            extent: Extent::Token(step),
            ..Self::new()
        }
    }

    /// Opt this spacer into presence tracking by binding it to a CRDT-style path.
    #[must_use]
    pub fn with_presence(mut self, path: Vec<PathSegment>) -> Self {
        self.presence_path = path;
        self
    }

    /// Gate this spacer behind a permission scope.
    #[must_use]
    pub fn with_permission_scope(mut self, scope: PermissionScope) -> Self {
        self.permission_scope = Some(scope);
        self
    }
}

impl Widget for Spacer {
    crate::provides!(
        dyn crate::permissions::capability::PermissionGated,
        dyn crate::presence::capability::PublishesPresence
    );

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let fixed = match self.extent {
            Extent::Flexible => None,
            Extent::Fixed(px) => Some(px),
            Extent::Token(step) => Some(theme.tokens.spacing.step(step)),
        };
        let style = match fixed {
            None => Style {
                flex_grow: 1.0,
                flex_shrink: 1.0,
                ..Default::default()
            },
            // A zero step is no gap at all: out of the flow, so the
            // container does not put its own gap on both sides of it.
            Some(px) if px <= 0.0 => super::when::removed_from_flow(),
            Some(px) => Style {
                flex_grow: 0.0,
                flex_shrink: 0.0,
                size: taffy::Size {
                    width: length(px),
                    height: length(px),
                },
                ..Default::default()
            },
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
        self.laid_out = true;
    }

    fn draw(
        &mut self,
        _scene: &mut Scene,
        _transform: Affine,
        _text_renderer: &mut TextRenderer,
        _theme: &Theme,
        _presence: &crate::presence::PresenceCtx,
    ) {
    }

    fn name(&self) -> &'static str {
        "Spacer"
    }

    fn test_id(&self) -> Option<&str> {
        self.id.as_deref()
    }

    /// `draw` is a no-op, so this is never dirty. With many Spacers in a
    /// tree (toolbars, padding rows), the default-true `needs_redraw`
    /// was pinning the whole tree to "always-dirty".
    fn needs_redraw(&self) -> bool {
        false
    }
    /// Dirty until the first `apply_layout`; a parent reflow after that is
    /// caught by the parent's own recursion.
    fn is_layout_dirty(&self) -> bool {
        !self.laid_out
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &crate::presence::PresenceCtx,
        permissions: &crate::permissions::PermissionContext,
    ) {
        if presence.should_cull(self.get_rect()) {
            return;
        }
        self.draw(scene, transform, text_renderer, theme, presence);

        crate::widget::draw_gate_and_peer_focus(
            self,
            scene,
            transform,
            theme,
            presence,
            permissions,
        );
    }
}

impl crate::presence::capability::PublishesPresence for Spacer {
    fn presence_key(&self) -> Option<PresenceKey> {
        PresenceKey::widget(&self.presence_path)
    }
}

impl crate::permissions::capability::PermissionGated for Spacer {
    fn permission_scope(&self) -> Option<PermissionScope> {
        self.permission_scope.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widget::{Column, Row, Text};
    use taffy::{AvailableSpace, Size};

    /// Same five-pass layout the `Column` tests use, with the real text
    /// renderer measuring the `Text` siblings.
    fn lay_out(root: &mut dyn Widget, width: f32, height: f32) -> (TaffyTree<()>, NodeId) {
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut nodes: std::collections::HashMap<NodeId, *mut dyn Widget> =
            std::collections::HashMap::new();
        let mut tr = TextRenderer::new();
        let node = root.build_layout(&mut taffy, &Theme::dark(), &mut nodes);
        taffy
            .compute_layout_with_measure(
                node,
                Size {
                    width: AvailableSpace::Definite(width),
                    height: AvailableSpace::Definite(height),
                },
                |known, available, node_id, _, _| {
                    if let Some(&ptr) = nodes.get(&node_id) {
                        unsafe { (*ptr).measure(known, available, &mut tr) }
                    } else {
                        Size::ZERO
                    }
                },
            )
            .unwrap();
        root.apply_layout(&taffy, node, Point::ZERO);
        (taffy, node)
    }

    fn child_rects(root: &dyn Widget) -> Vec<Rect> {
        root.children().iter().map(|c| c.get_rect()).collect()
    }

    #[test]
    fn spacers_push_or_separate_by_their_resolved_extent() {
        // Case `flexible_spacer_still_pushes_trailing_siblings_to_the_far_edge`.
        {
            let mut row = Row::new()
                .no_wrap()
                .push(Box::new(Text::new("lead")))
                .push(Box::new(Spacer::new()))
                .push(Box::new(Text::new("trail")));
            lay_out(&mut row, 600.0, 100.0);
            let rects = child_rects(&row);
            assert!(
                rects[1].width() > 300.0,
                "the spacer took the slack: {rects:?}"
            );
            assert!(
                (rects[2].x1 - 600.0).abs() < 0.5,
                "trailing text lands on the far edge: {rects:?}"
            );
        }

        // Case `fixed_spacer_is_exactly_its_pixels_in_a_row_and_does_not_grow`.
        {
            let mut row = Row::new()
                .no_wrap()
                .push(Box::new(Text::new("a")))
                .push(Box::new(Spacer::fixed(16.0)))
                .push(Box::new(Text::new("b")));
            lay_out(&mut row, 600.0, 100.0);
            let rects = child_rects(&row);
            assert_eq!(rects[1].width(), 16.0, "{rects:?}");
            assert_eq!(rects[1].height(), 16.0, "{rects:?}");
            assert!(
                rects[2].x1 < 300.0,
                "siblings stay packed at the start; the fixed spacer took no slack: {rects:?}"
            );
        }

        // Case `fixed_spacer_separates_column_children_by_its_pixels_plus_the_gap`.
        {
            let theme = Theme::dark();
            let mut column = Column::new()
                .with_intrinsic_height()
                .with_no_padding()
                .push(Box::new(Text::new("a")))
                .push(Box::new(Spacer::fixed(16.0)))
                .push(Box::new(Text::new("b")));
            lay_out(&mut column, 400.0, 400.0);
            let rects = child_rects(&column);
            assert_eq!(rects[1].height(), 16.0, "{rects:?}");
            let expected = rects[0].y1 + f64::from(theme.gap_standard) * 2.0 + 16.0;
            assert!(
                (rects[2].y0 - expected).abs() < 0.5,
                "b starts after a, two gaps and the spacer: {rects:?}"
            );
        }

        // Case `token_spacer_resolves_the_theme_step_and_follows_ui_scale`.
        {
            let mut taffy: TaffyTree<()> = TaffyTree::new();
            let mut nodes = std::collections::HashMap::new();
            let mut spacer = Spacer::token(SpacingStep::S4);
            let node = spacer.build_layout(&mut taffy, &Theme::dark(), &mut nodes);
            let size = taffy.style(node).unwrap().size;
            assert_eq!(size.width, length(16.0_f32));
            assert_eq!(size.height, length(16.0_f32));

            let scaled = Theme::dark().with_ui_scale(1.5);
            let mut taffy: TaffyTree<()> = TaffyTree::new();
            let mut spacer = Spacer::token(SpacingStep::S4);
            let node = spacer.build_layout(&mut taffy, &scaled, &mut nodes);
            assert_eq!(taffy.style(node).unwrap().size.height, length(24.0_f32));
        }

        // Case `zero_extent_spacer_at_the_origin_is_not_permanently_layout_dirty`.
        // A zero-size spacer laid out at the origin must not read as
        // "never laid out", or the host relayouts on every event forever.
        {
            let mut column = Column::new()
                .with_intrinsic_height()
                .with_no_padding()
                .with_gap(0.0)
                .push(Box::new(Spacer::token(SpacingStep::S0)))
                .push(Box::new(Text::new("body")));
            assert!(
                column.children()[0].is_layout_dirty(),
                "dirty before layout"
            );
            lay_out(&mut column, 400.0, 400.0);
            let spacer = column.children()[0];
            assert_eq!(spacer.get_rect(), Rect::ZERO, "zero size at the origin");
            assert!(!spacer.is_layout_dirty(), "laid out once is laid out");
        }

        // Case `negative_fixed_extent_is_clamped_to_zero`.
        {
            let mut taffy: TaffyTree<()> = TaffyTree::new();
            let mut nodes = std::collections::HashMap::new();
            let mut spacer = Spacer::fixed(-8.0);
            let node = spacer.build_layout(&mut taffy, &Theme::dark(), &mut nodes);
            assert_eq!(
                taffy.style(node).unwrap().display,
                taffy::Display::None,
                "clamped to nothing, and out of the flow"
            );
        }
    }
}
