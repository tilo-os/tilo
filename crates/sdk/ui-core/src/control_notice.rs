//! Why something the person just did did not run, said where they did it.
//!
//! A control the person pressed whose action the door refused (they may not
//! do it, the thing changed meanwhile, a remote failed) never fails with only
//! a log line, and never opens search over the window: the reason stands in
//! a small floating chip at that control, marked in the danger colour,
//! announced to assistive technology, and gone with the person's next press
//! or key. Something refused with no control on screen (an invocation an
//! agent, a host or a remote made for the person) is said in the window's
//! bar instead ([`crate::window_toolbar::notify`]).

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::tooltip::TooltipSide;

/// Why an invocation from a control did not run, and where that control
/// stands.
#[derive(Clone, Debug, PartialEq)]
pub struct ControlNotice {
    /// The reason, as the person reads it.
    pub text: String,
    /// The control the person acted on, in window coordinates.
    pub anchor: Rect,
}

impl ControlNotice {
    /// Paint the notice under its control, kept inside `viewport`.
    pub fn draw(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        viewport: Rect,
    ) {
        crate::widget::tooltip::draw_marked_tooltip_on(
            scene,
            transform,
            text_renderer,
            theme,
            (self.anchor, TooltipSide::Below),
            (&self.text, theme.tokens.colors.semantic.danger),
            viewport,
        );
    }
}

/// Say `reason` (a [`crate::bound_dispatch::refusal_reason`]) over the
/// window, for a refusal no control on screen asked
/// for: in the window's bar, announced, until the next notice replaces it.
pub fn say_in_window(reason: &str) {
    crate::window_toolbar::notify(Some(crate::window_toolbar::ToolbarNotice::new(
        reason,
        crate::widget::toast::ToastKind::Error,
    )));
}

/// The control the person acted on last, for anchoring what is said about
/// it: under the press `pressed_at` (the innermost node there that takes
/// focus, else the innermost node), else the node holding keyboard focus.
/// `rect_of` reads a node's rect and whether its widget takes focus.
pub(crate) fn acting_control(
    taffy: &taffy::TaffyTree<()>,
    root: Option<taffy::NodeId>,
    pressed_at: Option<Point>,
    focused: Option<taffy::NodeId>,
    rect_of: impl Fn(taffy::NodeId) -> Option<(Rect, bool)>,
) -> Option<Rect> {
    if let (Some(at), Some(root)) = (pressed_at, root) {
        let mut innermost = None;
        let mut control = None;
        let mut pending = vec![root];
        while let Some(node) = pending.pop() {
            let Some((rect, focusable)) = rect_of(node) else {
                continue;
            };
            if !rect.contains(at) || rect.area() <= 0.0 {
                continue;
            }
            innermost = Some(rect);
            if focusable {
                control = Some(rect);
            }
            if let Ok(children) = taffy.children(node) {
                pending.extend(children);
            }
        }
        if let Some(rect) = control.or(innermost) {
            return Some(rect);
        }
    }
    focused
        .and_then(|node| rect_of(node))
        .map(|(rect, _)| rect)
        .filter(|rect| rect.area() > 0.0)
}
