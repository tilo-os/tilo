//! Compact, shared key/controller/gesture legends.
//!
//! This is the component apps use beside menu items and controls. It also has
//! an immediate paint entry point for shell chrome, keeping the shape language
//! identical without constructing a temporary retained tree.

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Circle, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::Fill;
use winit::event::WindowEvent;

use crate::input::{ControllerBinding, InputBinding, KeyboardBinding};
use crate::interaction::{ControllerFamily, InteractionEnvironment, KeyboardFamily};
use crate::platform::PlatformConventions;
use crate::presence::PresenceCtx;
use crate::text_render::{TextRenderer, TextWeight};
use crate::theme::Theme;
use crate::widget::{TaffyTreeExt, Widget};

const HEIGHT: f64 = 22.0;
const CAP_GAP: f64 = 3.0;
const CAP_PAD_X: f64 = 6.0;
const MIN_CAP_WIDTH: f64 = 22.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HintShape {
    Keycap,
    RoundButton,
    Gesture,
}

pub struct InputHint {
    binding: InputBinding,
    platform: PlatformConventions,
    environment: InteractionEnvironment,
    rect: Rect,
}

/// Shared vector controller/Joy-Con silhouette used in device pickers and
/// shortcut help. It intentionally uses theme strokes instead of bundled
/// vendor artwork, so it stays crisp at every scale and in high contrast.
pub struct InputDeviceGlyph;

impl InputDeviceGlyph {
    pub fn paint_controller(
        scene: &mut Scene,
        transform: Affine,
        rect: Rect,
        family: ControllerFamily,
        theme: &Theme,
    ) {
        if rect.width() <= 0.0 || rect.height() <= 0.0 {
            return;
        }
        let colors = &theme.tokens.colors;
        let stroke = Stroke::new(1.5);
        let ink = colors.text_primary;
        let quiet = colors.neutral[2];
        if matches!(
            family,
            ControllerFamily::JoyConLeft | ControllerFamily::JoyConRight
        ) {
            let body = RoundedRect::from_rect(rect, rect.width() * 0.45);
            scene.fill(Fill::NonZero, transform, quiet, None, &body);
            scene.stroke(&stroke, transform, ink, None, &body);
            let upper = Circle::new(
                Point::new(rect.center().x, rect.y0 + rect.height() * 0.28),
                rect.width() * 0.16,
            );
            scene.stroke(&stroke, transform, ink, None, &upper);
            let lower_y = rect.y0 + rect.height() * 0.66;
            for (dx, dy) in [(0.0, -6.0), (6.0, 0.0), (0.0, 6.0), (-6.0, 0.0)] {
                scene.fill(
                    Fill::NonZero,
                    transform,
                    ink,
                    None,
                    &Circle::new(Point::new(rect.center().x + dx, lower_y + dy), 1.8),
                );
            }
            return;
        }
        if family == ControllerFamily::JoyConPair {
            let gap = rect.width() * 0.08;
            let half = (rect.width() - gap) / 2.0;
            let left = Rect::new(rect.x0, rect.y0, rect.x0 + half, rect.y1);
            let right = Rect::new(left.x1 + gap, rect.y0, rect.x1, rect.y1);
            Self::paint_controller(scene, transform, left, ControllerFamily::JoyConLeft, theme);
            Self::paint_controller(
                scene,
                transform,
                right,
                ControllerFamily::JoyConRight,
                theme,
            );
            return;
        }

        let body = RoundedRect::from_rect(rect, rect.height() * 0.34);
        scene.fill(Fill::NonZero, transform, quiet, None, &body);
        scene.stroke(&stroke, transform, ink, None, &body);
        let cy = rect.center().y;
        let lx = rect.x0 + rect.width() * 0.28;
        let rx = rect.x0 + rect.width() * 0.72;
        let d = rect.height() * 0.12;
        scene.stroke(
            &Stroke::new(2.2),
            transform,
            ink,
            None,
            &Line::new(Point::new(lx - d, cy), Point::new(lx + d, cy)),
        );
        scene.stroke(
            &Stroke::new(2.2),
            transform,
            ink,
            None,
            &Line::new(Point::new(lx, cy - d), Point::new(lx, cy + d)),
        );
        for (dx, dy) in [(0.0, -d), (d, 0.0), (0.0, d), (-d, 0.0)] {
            scene.fill(
                Fill::NonZero,
                transform,
                ink,
                None,
                &Circle::new(Point::new(rx + dx, cy + dy), 2.2),
            );
        }
        match family {
            ControllerFamily::PlayStation => {
                let pad = Rect::new(
                    rect.center().x - rect.width() * 0.1,
                    rect.y0 + rect.height() * 0.2,
                    rect.center().x + rect.width() * 0.1,
                    rect.y0 + rect.height() * 0.38,
                );
                scene.stroke(
                    &Stroke::new(1.0),
                    transform,
                    ink,
                    None,
                    &RoundedRect::from_rect(pad, 2.0),
                );
            }
            ControllerFamily::Nintendo => {
                for x in [rect.center().x - 5.0, rect.center().x + 5.0] {
                    scene.stroke(
                        &Stroke::new(1.0),
                        transform,
                        ink,
                        None,
                        &Circle::new(Point::new(x, rect.y0 + rect.height() * 0.72), 3.0),
                    );
                }
            }
            ControllerFamily::Xbox => {
                scene.stroke(
                    &Stroke::new(1.0),
                    transform,
                    ink,
                    None,
                    &Circle::new(
                        Point::new(rect.center().x, rect.y0 + rect.height() * 0.3),
                        3.5,
                    ),
                );
            }
            ControllerFamily::Generic
            | ControllerFamily::JoyConLeft
            | ControllerFamily::JoyConRight
            | ControllerFamily::JoyConPair => {}
        }
    }
}

impl InputHint {
    pub fn new(binding: InputBinding) -> Self {
        Self {
            binding,
            platform: PlatformConventions::default(),
            environment: InteractionEnvironment::default(),
            rect: Rect::ZERO,
        }
    }

    pub fn keyboard(binding: KeyboardBinding) -> Self {
        Self::new(InputBinding::Keyboard(binding))
    }

    fn controller_family(&self) -> ControllerFamily {
        self.environment
            .controller
            .unwrap_or(ControllerFamily::Generic)
    }

    fn tokens(&self) -> Vec<(String, HintShape)> {
        tokens_for(
            &self.binding,
            self.platform,
            self.environment.keyboard_family,
            self.controller_family(),
        )
    }

    pub fn accessible_label(&self) -> String {
        self.tokens()
            .into_iter()
            .map(|(label, _)| label)
            .collect::<Vec<_>>()
            .join(" + ")
    }

    /// Natural width using the real text metrics. Useful to immediate-mode
    /// hosts that need to align the hint to a trailing edge.
    pub fn measure_width(
        binding: &InputBinding,
        platform: PlatformConventions,
        keyboard_family: KeyboardFamily,
        family: ControllerFamily,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) -> f64 {
        token_rects(
            &tokens_for(binding, platform, keyboard_family, family),
            Point::ZERO,
            text_renderer,
            theme,
        )
        .last()
        .map_or(0.0, |(_, rect)| rect.x1)
    }

    /// Paint the same component used by the retained widget at `origin`.
    pub fn paint_at(
        scene: &mut Scene,
        transform: Affine,
        origin: Point,
        binding: &InputBinding,
        platform: PlatformConventions,
        keyboard_family: KeyboardFamily,
        family: ControllerFamily,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) -> f64 {
        let tokens = tokens_for(binding, platform, keyboard_family, family);
        let rects = token_rects(&tokens, origin, text_renderer, theme);
        for ((label, shape), (_, rect)) in tokens.iter().zip(&rects) {
            paint_token(scene, transform, *rect, label, *shape, text_renderer, theme);
        }
        rects.last().map_or(0.0, |(_, rect)| rect.x1 - origin.x)
    }

    pub fn measure_keycap_labels(
        labels: &[String],
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) -> f64 {
        let tokens: Vec<_> = labels
            .iter()
            .cloned()
            .map(|label| (label, HintShape::Keycap))
            .collect();
        token_rects(&tokens, Point::ZERO, text_renderer, theme)
            .last()
            .map_or(0.0, |(_, rect)| rect.x1)
    }

    pub fn paint_keycap_labels_at(
        scene: &mut Scene,
        transform: Affine,
        origin: Point,
        labels: &[String],
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) -> f64 {
        let tokens: Vec<_> = labels
            .iter()
            .cloned()
            .map(|label| (label, HintShape::Keycap))
            .collect();
        let rects = token_rects(&tokens, origin, text_renderer, theme);
        for ((label, shape), (_, rect)) in tokens.iter().zip(&rects) {
            paint_token(scene, transform, *rect, label, *shape, text_renderer, theme);
        }
        rects.last().map_or(0.0, |(_, rect)| rect.x1 - origin.x)
    }

    /// Paint two alternative keyboard chords without repeating their shared
    /// modifiers. Separators are text rather than extra caps, so a pair reads
    /// as `Super + Left or Right`, not as one long row of unrelated keys.
    pub fn paint_keyboard_choice_at(
        scene: &mut Scene,
        transform: Affine,
        origin: Point,
        first: &KeyboardBinding,
        second: &KeyboardBinding,
        platform: PlatformConventions,
        keyboard_family: KeyboardFamily,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) -> [Rect; 2] {
        let first_labels = first.keycap_labels_for(platform, keyboard_family);
        let second_labels = second.keycap_labels_for(platform, keyboard_family);
        let share_modifiers = first.modifiers == second.modifiers;
        let mut x = origin.x;

        let first_rect;
        let second_rect;
        if share_modifiers {
            let modifier_count = first_labels.len().saturating_sub(1);
            if modifier_count > 0 {
                x += Self::paint_keycap_labels_at(
                    scene,
                    transform,
                    Point::new(x, origin.y),
                    &first_labels[..modifier_count],
                    text_renderer,
                    theme,
                );
                x += paint_separator(
                    scene,
                    transform,
                    Point::new(x + 7.0, origin.y),
                    "+",
                    text_renderer,
                    theme,
                ) + 14.0;
            }
            let first_x = x;
            x += Self::paint_keycap_labels_at(
                scene,
                transform,
                Point::new(x, origin.y),
                &first_labels[modifier_count..],
                text_renderer,
                theme,
            );
            first_rect = Rect::new(first_x, origin.y, x, origin.y + HEIGHT);
            x += paint_separator(
                scene,
                transform,
                Point::new(x + 10.0, origin.y),
                "or",
                text_renderer,
                theme,
            ) + 20.0;
            let second_x = x;
            x += Self::paint_keycap_labels_at(
                scene,
                transform,
                Point::new(x, origin.y),
                &second_labels[modifier_count..],
                text_renderer,
                theme,
            );
            second_rect = Rect::new(second_x, origin.y, x, origin.y + HEIGHT);
        } else {
            let first_x = x;
            x += Self::paint_keycap_labels_at(
                scene,
                transform,
                Point::new(x, origin.y),
                &first_labels,
                text_renderer,
                theme,
            );
            first_rect = Rect::new(first_x, origin.y, x, origin.y + HEIGHT);
            x += paint_separator(
                scene,
                transform,
                Point::new(x + 10.0, origin.y),
                "or",
                text_renderer,
                theme,
            ) + 20.0;
            let second_x = x;
            x += Self::paint_keycap_labels_at(
                scene,
                transform,
                Point::new(x, origin.y),
                &second_labels,
                text_renderer,
                theme,
            );
            second_rect = Rect::new(second_x, origin.y, x, origin.y + HEIGHT);
        }
        [first_rect, second_rect]
    }

    /// Controller counterpart to [`Self::paint_keyboard_choice_at`]. Shared
    /// hold buttons such as Guide/Home are shown once before the alternatives.
    pub fn paint_controller_choice_at(
        scene: &mut Scene,
        transform: Affine,
        origin: Point,
        first: &ControllerBinding,
        second: &ControllerBinding,
        platform: PlatformConventions,
        keyboard_family: KeyboardFamily,
        family: ControllerFamily,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) -> f64 {
        if first == second {
            return Self::paint_at(
                scene,
                transform,
                origin,
                &InputBinding::Controller(first.clone()),
                platform,
                keyboard_family,
                family,
                text_renderer,
                theme,
            );
        }

        let shared: Vec<_> = first
            .buttons
            .iter()
            .copied()
            .filter(|button| second.buttons.contains(button))
            .collect();
        let first_only: Vec<_> = first
            .buttons
            .iter()
            .copied()
            .filter(|button| !shared.contains(button))
            .collect();
        let second_only: Vec<_> = second
            .buttons
            .iter()
            .copied()
            .filter(|button| !shared.contains(button))
            .collect();
        let mut x = origin.x;

        if !shared.is_empty() {
            x += Self::paint_at(
                scene,
                transform,
                Point::new(x, origin.y),
                &InputBinding::Controller(ControllerBinding::new(shared)),
                platform,
                keyboard_family,
                family,
                text_renderer,
                theme,
            );
            x += paint_separator(
                scene,
                transform,
                Point::new(x + 7.0, origin.y),
                "+",
                text_renderer,
                theme,
            ) + 14.0;
        }

        x += Self::paint_at(
            scene,
            transform,
            Point::new(x, origin.y),
            &InputBinding::Controller(ControllerBinding::new(first_only)),
            platform,
            keyboard_family,
            family,
            text_renderer,
            theme,
        );
        x += paint_separator(
            scene,
            transform,
            Point::new(x + 10.0, origin.y),
            "or",
            text_renderer,
            theme,
        ) + 20.0;
        x += Self::paint_at(
            scene,
            transform,
            Point::new(x, origin.y),
            &InputBinding::Controller(ControllerBinding::new(second_only)),
            platform,
            keyboard_family,
            family,
            text_renderer,
            theme,
        );
        x - origin.x
    }
}

fn paint_separator(
    scene: &mut Scene,
    transform: Affine,
    origin: Point,
    label: &str,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
) -> f64 {
    let size = theme.tokens.typography.scale.xs;
    let (width, height) = text_renderer.measure(label, size, None);
    text_renderer.draw_text(
        scene,
        label,
        size,
        theme.tokens.colors.text_dim,
        translated(
            transform,
            origin.x,
            origin.y + (HEIGHT - f64::from(height)) / 2.0,
        ),
        None,
    );
    f64::from(width)
}

fn tokens_for(
    binding: &InputBinding,
    platform: PlatformConventions,
    keyboard_family: KeyboardFamily,
    family: ControllerFamily,
) -> Vec<(String, HintShape)> {
    match binding {
        InputBinding::Keyboard(binding) => binding
            .keycap_labels_for(platform, keyboard_family)
            .into_iter()
            .map(|label| (label, HintShape::Keycap))
            .collect(),
        InputBinding::Controller(binding) => binding
            .buttons
            .iter()
            .filter(|button| **button == crate::input::ControllerButton::Guide)
            .chain(
                binding
                    .buttons
                    .iter()
                    .filter(|button| **button != crate::input::ControllerButton::Guide),
            )
            .map(|button| (button.label(family).to_owned(), HintShape::RoundButton))
            .collect(),
        InputBinding::Touch(gesture) => vec![(gesture.compact_label(), HintShape::Gesture)],
    }
}

fn token_rects(
    tokens: &[(String, HintShape)],
    origin: Point,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
) -> Vec<(HintShape, Rect)> {
    let mut x = origin.x;
    tokens
        .iter()
        .map(|(label, shape)| {
            let size = token_text_size(label, theme);
            let (text_width, _) = text_renderer.measure(label, size, None);
            let width = match shape {
                HintShape::RoundButton => (f64::from(text_width) + CAP_PAD_X).max(HEIGHT),
                HintShape::Keycap | HintShape::Gesture => {
                    (f64::from(text_width) + CAP_PAD_X * 2.0).max(MIN_CAP_WIDTH)
                }
            };
            let rect = Rect::new(x, origin.y, x + width, origin.y + HEIGHT);
            x += width + CAP_GAP;
            (*shape, rect)
        })
        .collect()
}

fn paint_token(
    scene: &mut Scene,
    transform: Affine,
    rect: Rect,
    label: &str,
    shape: HintShape,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
) {
    let colors = &theme.tokens.colors;
    let radius = match shape {
        HintShape::RoundButton => rect.height() / 2.0,
        HintShape::Gesture => 11.0,
        HintShape::Keycap => 5.0,
    };
    let outline = RoundedRect::from_rect(rect, radius);
    scene.fill(Fill::NonZero, transform, colors.neutral[2], None, &outline);
    scene.stroke(
        &Stroke::new(1.0),
        transform,
        colors.neutral[5],
        None,
        &outline,
    );
    let size = token_text_size(label, theme);
    let (tw, th) = text_renderer.measure(label, size, None);
    text_renderer.draw_text_weighted(
        scene,
        label,
        size,
        colors.text_primary,
        translated(
            transform,
            rect.center().x - f64::from(tw) / 2.0,
            rect.center().y - f64::from(th) / 2.0,
        ),
        None,
        TextWeight::Semibold,
    );
}

fn token_text_size(label: &str, theme: &Theme) -> f32 {
    if matches!(label, "←" | "→" | "↑" | "↓" | "×" | "○" | "□" | "△") {
        theme.tokens.typography.scale.sm
    } else {
        theme.tokens.typography.scale.xs
    }
}

#[allow(
    clippy::arithmetic_side_effects,
    reason = "Affine composition is bounded f64 UI geometry"
)]
fn translated(transform: Affine, x: f64, y: f64) -> Affine {
    transform * Affine::translate((x, y))
}

impl Widget for InputHint {
    fn set_platform_conventions(&mut self, conventions: PlatformConventions) {
        self.platform = conventions;
        self.environment.keyboard_family =
            if conventions.platform() == crate::platform::Platform::MacOs {
                KeyboardFamily::Apple
            } else {
                KeyboardFamily::Pc
            };
    }

    fn set_interaction_environment(&mut self, environment: InteractionEnvironment) {
        self.environment = environment;
    }

    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let approximate = self
            .tokens()
            .iter()
            .map(|(label, shape)| match shape {
                HintShape::RoundButton => {
                    (label.chars().count() as f64 * 7.0 + CAP_PAD_X).max(HEIGHT)
                }
                HintShape::Keycap | HintShape::Gesture => {
                    (label.chars().count() as f64 * 7.0 + CAP_PAD_X * 2.0).max(MIN_CAP_WIDTH)
                }
            })
            .sum::<f64>()
            + CAP_GAP * self.tokens().len().saturating_sub(1) as f64;
        let node = taffy.leaf_node(Style {
            size: Size {
                width: Dimension::length(approximate as f32),
                height: Dimension::length(f64::from(theme.tokens.spacing.s6).max(HEIGHT) as f32),
            },
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn measure(
        &mut self,
        _known: Size<Option<f32>>,
        _available: Size<taffy::AvailableSpace>,
        text_renderer: &mut TextRenderer,
    ) -> Size<f32> {
        let theme = Theme::dark();
        Size {
            width: Self::measure_width(
                &self.binding,
                self.platform,
                self.environment.keyboard_family,
                self.controller_family(),
                text_renderer,
                &theme,
            ) as f32,
            height: HEIGHT as f32,
        }
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x + layout.size.width),
            origin.y + f64::from(layout.location.y + layout.size.height),
        );
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        Self::paint_at(
            scene,
            transform,
            Point::new(self.rect.x0, self.rect.center().y - HEIGHT / 2.0),
            &self.binding,
            self.platform,
            self.environment.keyboard_family,
            self.controller_family(),
            text_renderer,
            theme,
        );
    }

    fn handle_event(
        &mut self,
        _event: &WindowEvent,
        _scale_factor: f64,
        _modifiers: &winit::event::Modifiers,
        _state: &mut AutoCommit,
        _path: &ObjId,
        _handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
    }

    fn name(&self) -> &'static str {
        "InputHint"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        Some(
            crate::a11y::AccessNode::new(crate::a11y::Role::StaticText)
                .with_value(self.accessible_label()),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::{ControllerBinding, ControllerButton, KeyLegend, KeyboardModifier};
    use crate::platform::Platform;

    #[test]
    fn hint_accessibility_uses_platform_and_controller_legends() {
        let mut keyboard = InputHint::keyboard(KeyboardBinding::new(
            [KeyboardModifier::Command, KeyboardModifier::Shift],
            KeyLegend::Enter,
        ));
        keyboard.set_platform_conventions(PlatformConventions::new(Platform::MacOs));
        assert_eq!(keyboard.accessible_label(), "⌘ + ⇧ + ↩");

        let mut controller = InputHint::new(InputBinding::Controller(ControllerBinding::new([
            ControllerButton::South,
        ])));
        controller.set_interaction_environment(
            InteractionEnvironment::POINTER.with_controller(ControllerFamily::Nintendo),
        );
        assert_eq!(controller.accessible_label(), "B");
    }
}
