//! [`StatusLog`] — a fixed-height, auto-tailing status view.
//!
//! Renders the **tail** of a list of log lines inside a bordered, clipped
//! box: as new lines arrive the newest stay visible (like `tail -f`). It
//! is a *leaf* widget — no children — so it trivially mirrors the taffy
//! tree for the App's focusable-order traversal. It is not focusable and
//! consumes no events.
//!
//! Why a bespoke widget rather than `ui::Scrollable`? The install output
//! arrives in one synchronous batch (see [`crate::install`]) and we want
//! a bounded box that always shows the latest progress without any
//! viewport/content-size wiring. Tailing is exactly the installer's need.

use std::collections::HashMap;
use ui::widget::TaffyTreeExt;

use taffy::{NodeId, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke, Vec2};
use vello::peniko::{BlendMode, Fill};

use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;

/// Font size for log lines.
const LINE_SIZE: f32 = 13.0;
/// Extra vertical gap between lines.
const LINE_GAP: f32 = 4.0;
/// Inner padding inside the log box.
const PAD: f64 = 8.0;

pub struct StatusLog {
    lines: Vec<String>,
    height: f32,
    rect: Rect,
}

impl StatusLog {
    /// Build a log box `height` logical px tall showing `lines` (tailed).
    pub fn new(lines: Vec<String>, height: f32) -> Self {
        Self {
            lines,
            height,
            rect: Rect::ZERO,
        }
    }

    fn latest_announcement(&self) -> Option<&str> {
        self.lines
            .iter()
            .rev()
            .map(String::as_str)
            .find(|line| !line.trim().is_empty())
    }
}

impl Widget for StatusLog {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.widget_leaf(crate::full_width_row(self.height), node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        // `layout` only errors for a `NodeId` from a different tree, which
        // never happens here since `node` is always the id this same widget
        // received from its own `build_layout` call.
        #[allow(clippy::unwrap_used, reason = "node always belongs to this tree")]
        let l = taffy.layout(node).unwrap();
        self.rect = ui::widget::placed(l, origin);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        let shape = RoundedRect::from_rect(self.rect, theme.radius_standard);
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.content,
            None,
            &shape,
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            theme.border_standard,
            None,
            &shape,
        );

        // Clip glyphs to the box so long lines never bleed out.
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, &shape);
        let max_w = ui::num::f32_of_f64(self.rect.width() - PAD * 2.0);
        let inner_h = self.rect.height() - PAD * 2.0;

        // Tail: walk from the newest line upward, summing each line's WRAPPED
        // height (measured at the box width) until the box is full. A line that
        // wraps to N visual rows must advance y by all N — otherwise the next
        // line overdraws the wrapped continuation (the cause of the overlap).
        let mut first = self.lines.len();
        let mut used = 0.0_f64;
        for (i, line) in self.lines.iter().enumerate().rev() {
            let (_, h) = text_renderer.measure(line, LINE_SIZE, Some(max_w));
            let need = f64::from(h) + f64::from(LINE_GAP);
            if used + need > inner_h && first != self.lines.len() {
                break;
            }
            used += need;
            first = i;
        }

        let mut y = self.rect.y0 + PAD;
        for line in self.lines.iter().skip(first) {
            let (_, h) = text_renderer.measure(line, LINE_SIZE, Some(max_w));
            text_renderer.draw_text(
                scene,
                line,
                LINE_SIZE,
                theme.text_primary,
                transform.pre_translate(Vec2::new(self.rect.x0 + PAD, y)),
                Some(max_w),
            );
            y += f64::from(h) + f64::from(LINE_GAP);
        }
        scene.pop_layer();
    }

    fn name(&self) -> &'static str {
        "StatusLog"
    }

    fn test_id(&self) -> Option<&str> {
        Some("install_status_log")
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, Politeness, Role};

        let latest = self.latest_announcement()?;
        let lower = latest.to_ascii_lowercase();
        let politeness = if lower.contains("error")
            || lower.contains("failed")
            || lower.contains("touch your security key")
        {
            Politeness::Assertive
        } else {
            Politeness::Polite
        };
        Some(
            AccessNode::new(Role::StaticText)
                .with_semantic_key("installer.install-status")
                .with_name(latest)
                .with_live(politeness),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_log_live_region_politeness_follows_severity() {
        // Phase `newest_progress_line_is_a_stable_polite_live_region`.
        {
            let log = StatusLog::new(
                vec![
                    "Preparing encrypted storage…".into(),
                    "Creating your account…".into(),
                ],
                200.0,
            );
            let node = log.accessibility().expect("status semantics");
            assert_eq!(node.name.as_deref(), Some("Creating your account…"));
            assert_eq!(node.live, Some(ui::a11y::Politeness::Polite));
            assert_eq!(
                node.semantic_key.as_deref(),
                Some("installer.install-status")
            );
        }
        // Phase `failures_and_hardware_touch_prompts_are_assertive`.
        {
            for line in [
                "error: target identity changed",
                "Touch your security key to continue…",
            ] {
                let log = StatusLog::new(vec![line.into()], 200.0);
                assert_eq!(
                    log.accessibility().and_then(|node| node.live),
                    Some(ui::a11y::Politeness::Assertive),
                    "did not announce {line:?} immediately"
                );
            }
        }
    }
}
