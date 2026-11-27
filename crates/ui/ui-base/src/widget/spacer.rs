use crate::presence::{PathSegment, PresenceKey};
use crate::theme::Theme;
use crate::tokens::SpacingStep;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;
use taffy::{NodeId, Style, TaffyTree};
use vello::kurbo::{Point, Rect};

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

#[crate::component::not_a_component(layout, "the language's `spacer`")]
pub struct Spacer {
    rect: Rect,
    /// Set by the first `apply_layout`. A separate flag rather than a
    /// `Rect::ZERO` sentinel because a zero-extent spacer at the origin
    /// (`fixed(0.0)`, `token(SpacingStep::S0)`) is a legitimate rect.
    laid_out: bool,
    extent: Extent,
    id: Option<String>,
    presence_path: Vec<PathSegment>,
}

impl Default for Spacer {
    fn default() -> Self {
        Self::new()
    }
}

impl Spacer {
    pub fn new() -> Self {
        Self {
            rect: Rect::ZERO,
            laid_out: false,
            extent: Extent::Flexible,
            id: None,
            presence_path: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = Some(id.into());
        self
    }

    /// A fixed gap of exactly `px` logical pixels on both axes, so it reads as a vertical gap
    /// in a `Column` and a horizontal one in a `Row` without a second constructor.
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

    #[must_use]
    pub fn with_presence(mut self, path: Vec<PathSegment>) -> Self {
        self.presence_path = path;
        self
    }
}

impl Widget for Spacer {
    crate::provides!(dyn crate::presence::capability::PublishesPresence);

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
            Some(px) if px <= 0.0 => super::layout::removed_from_flow(),
            Some(px) => Style {
                flex_grow: 0.0,
                flex_shrink: 0.0,
                size: taffy::Size::from_lengths(px, px),
                ..Default::default()
            },
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
        self.laid_out = true;
    }

    fn draw(&mut self, ctx: &mut crate::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, _text_renderer, theme, mut env) = ctx.parts();
        crate::widget::draw_over(
            self,
            &mut crate::draw::DrawCtx::new(scene, transform, _text_renderer, theme, env.clone()),
        );
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
}

impl crate::presence::capability::PublishesPresence for Spacer {
    fn presence_key(&self) -> Option<PresenceKey> {
        PresenceKey::widget(&self.presence_path)
    }
}
