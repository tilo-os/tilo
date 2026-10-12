//! Compact, shared key/controller/gesture legends.
//!
//! This is the component apps use beside menu items and controls. It also has
//! an immediate paint entry point for shell chrome, keeping the shape language
//! identical without constructing a temporary retained tree.

use std::collections::HashMap;

use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Circle, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::Fill;

use crate::input::{ControllerBinding, InputBinding, KeyboardBinding};
use crate::interaction::{ControllerFamily, InteractionEnvironment, KeyboardFamily};
use crate::paint::PaintCtx;
use crate::platform::PlatformConventions;
use crate::presence::PresenceCtx;
use crate::text_render::{TextRenderer, TextStyle, TextWeight};
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::{TaffyTreeExt, Widget};

/// Height of a painted hint row; hosts that centre a hint in their own band
/// (the shortcut recorder) offset by half of this rather than a literal.
pub(crate) const HEIGHT: f64 = 22.0;
const CAP_GAP: f64 = 3.0;
const CAP_PAD_X: f64 = 6.0;
const MIN_CAP_WIDTH: f64 = 22.0;

fn token_height(theme: &Theme) -> f64 {
    HEIGHT.max(f64::from(theme.tokens.typography.size(TextRole::Caption)) + 8.0)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HintShape {
    Keycap,
    RoundButton,
    Gesture,
}

/// The platform and device families that decide how a binding is labelled:
/// `⌘` vs `Ctrl`, `A` vs `✕`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LegendConventions {
    pub platform: PlatformConventions,
    pub keyboard: KeyboardFamily,
    pub controller: ControllerFamily,
}

impl LegendConventions {
    /// Label for `environment`'s keyboard and controller, falling back to a
    /// generic controller when none is connected.
    pub fn new(platform: PlatformConventions, environment: &InteractionEnvironment) -> Self {
        Self {
            platform,
            keyboard: environment.keyboard_family,
            controller: environment.controller.unwrap_or(ControllerFamily::Generic),
        }
    }
}

pub struct InputHint {
    binding: InputBinding,
    platform: PlatformConventions,
    environment: InteractionEnvironment,
    rect: Rect,
    layout_theme: Theme,
}

/// Shared vector controller/Joy-Con silhouette used in device pickers and
/// shortcut help. It intentionally uses theme strokes instead of bundled
/// vendor artwork, so it stays crisp at every scale and in high contrast.
pub struct InputDeviceGlyph;

impl InputDeviceGlyph {
    #[allow(
        clippy::too_many_lines,
        reason = "the branches form one compact vector glyph; splitting them would obscure the shared geometry"
    )]
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
        // A lift off the ground, not a ramp slot (HIG §9.2).
        let quiet = colors.lift(1.0);
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
            layout_theme: Theme::dark(),
        }
    }

    pub fn keyboard(binding: KeyboardBinding) -> Self {
        Self::new(InputBinding::Keyboard(binding))
    }

    fn conventions(&self) -> LegendConventions {
        LegendConventions::new(self.platform, &self.environment)
    }

    fn tokens(&self) -> Vec<(String, HintShape)> {
        tokens_for(&self.binding, self.conventions())
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
        conventions: LegendConventions,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) -> f64 {
        token_rects(
            &tokens_for(binding, conventions),
            Point::ZERO,
            text_renderer,
            theme,
        )
        .last()
        .map_or(0.0, |(_, rect)| rect.x1)
    }

    /// Paint the same component used by the retained widget at `origin`.
    pub fn paint_at(
        ctx: &mut PaintCtx<'_>,
        origin: Point,
        binding: &InputBinding,
        conventions: LegendConventions,
    ) -> f64 {
        let tokens = tokens_for(binding, conventions);
        let rects = token_rects(&tokens, origin, ctx.text, ctx.theme);
        for ((label, shape), (_, rect)) in tokens.iter().zip(&rects) {
            paint_token(ctx, *rect, label, *shape);
        }
        rects.last().map_or(0.0, |(_, rect)| rect.x1 - origin.x)
    }

    /// The height of one keycap in `theme`, for centring a row of them.
    pub fn keycap_height(theme: &Theme) -> f64 {
        token_height(theme)
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
        let mut ctx = PaintCtx::new(scene, transform, text_renderer, theme);
        for ((label, shape), (_, rect)) in tokens.iter().zip(&rects) {
            paint_token(&mut ctx, *rect, label, *shape);
        }
        rects.last().map_or(0.0, |(_, rect)| rect.x1 - origin.x)
    }

    /// Paint two alternative keyboard chords without repeating their shared
    /// modifiers. Separators are text rather than extra caps, so a pair reads
    /// as `Super + Left or Right`, not as one long row of unrelated keys.
    pub fn paint_keyboard_choice_at(
        ctx: &mut PaintCtx<'_>,
        origin: Point,
        first: &KeyboardBinding,
        second: &KeyboardBinding,
        conventions: LegendConventions,
    ) -> [Rect; 2] {
        let LegendConventions {
            platform, keyboard, ..
        } = conventions;
        let theme = ctx.theme;
        let first_labels = first.keycap_labels_for(platform, keyboard);
        let second_labels = second.keycap_labels_for(platform, keyboard);
        let share_modifiers = first.modifiers == second.modifiers;
        let mut x = origin.x;

        let first_rect;
        let second_rect;
        if share_modifiers {
            let modifier_count = first_labels
                .len()
                .saturating_sub(1)
                .min(second_labels.len().saturating_sub(1));
            if modifier_count > 0 {
                x += Self::paint_keycap_labels_at(
                    ctx.scene,
                    ctx.transform,
                    Point::new(x, origin.y),
                    first_labels.get(..modifier_count).unwrap_or_default(),
                    ctx.text,
                    theme,
                );
                x += paint_separator(ctx, Point::new(x + 7.0, origin.y), "+") + 14.0;
            }
            let first_x = x;
            x += Self::paint_keycap_labels_at(
                ctx.scene,
                ctx.transform,
                Point::new(x, origin.y),
                first_labels.get(modifier_count..).unwrap_or_default(),
                ctx.text,
                theme,
            );
            first_rect = Rect::new(first_x, origin.y, x, origin.y + token_height(theme));
            x += paint_separator(ctx, Point::new(x + 10.0, origin.y), "or") + 20.0;
            let second_x = x;
            x += Self::paint_keycap_labels_at(
                ctx.scene,
                ctx.transform,
                Point::new(x, origin.y),
                second_labels.get(modifier_count..).unwrap_or_default(),
                ctx.text,
                theme,
            );
            second_rect = Rect::new(second_x, origin.y, x, origin.y + token_height(theme));
        } else {
            let first_x = x;
            x += Self::paint_keycap_labels_at(
                ctx.scene,
                ctx.transform,
                Point::new(x, origin.y),
                &first_labels,
                ctx.text,
                theme,
            );
            first_rect = Rect::new(first_x, origin.y, x, origin.y + token_height(theme));
            x += paint_separator(ctx, Point::new(x + 10.0, origin.y), "or") + 20.0;
            let second_x = x;
            x += Self::paint_keycap_labels_at(
                ctx.scene,
                ctx.transform,
                Point::new(x, origin.y),
                &second_labels,
                ctx.text,
                theme,
            );
            second_rect = Rect::new(second_x, origin.y, x, origin.y + token_height(theme));
        }
        [first_rect, second_rect]
    }

    /// Controller counterpart to [`Self::paint_keyboard_choice_at`]. Shared
    /// hold buttons such as Guide/Home are shown once before the alternatives.
    pub fn paint_controller_choice_at(
        ctx: &mut PaintCtx<'_>,
        origin: Point,
        first: &ControllerBinding,
        second: &ControllerBinding,
        conventions: LegendConventions,
    ) -> f64 {
        if first == second {
            return Self::paint_at(
                ctx,
                origin,
                &InputBinding::Controller(first.clone()),
                conventions,
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
                ctx,
                Point::new(x, origin.y),
                &InputBinding::Controller(ControllerBinding::new(shared)),
                conventions,
            );
            x += paint_separator(ctx, Point::new(x + 7.0, origin.y), "+") + 14.0;
        }

        x += Self::paint_at(
            ctx,
            Point::new(x, origin.y),
            &InputBinding::Controller(ControllerBinding::new(first_only)),
            conventions,
        );
        x += paint_separator(ctx, Point::new(x + 10.0, origin.y), "or") + 20.0;
        x += Self::paint_at(
            ctx,
            Point::new(x, origin.y),
            &InputBinding::Controller(ControllerBinding::new(second_only)),
            conventions,
        );
        x - origin.x
    }
}

fn paint_separator(ctx: &mut PaintCtx<'_>, origin: Point, label: &str) -> f64 {
    let (text_renderer, theme) = (&mut *ctx.text, ctx.theme);
    let style = theme.text(TextRole::Caption);
    let (width, height) = text_renderer.measure_style(label, None, &style);
    ctx.text(
        label,
        &style.clone(),
        theme.tokens.colors.text_dim,
        Point::new(
            origin.x,
            origin.y + (token_height(theme) - f64::from(height)) / 2.0,
        ),
        None,
    );
    f64::from(width)
}

fn tokens_for(binding: &InputBinding, conventions: LegendConventions) -> Vec<(String, HintShape)> {
    let LegendConventions {
        platform,
        keyboard: keyboard_family,
        controller: family,
    } = conventions;
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
    let height = token_height(theme);
    tokens
        .iter()
        .map(|(label, shape)| {
            let style = token_text_style(theme);
            let (text_width, _) = text_renderer.measure_style(label, None, &style);
            let width = match shape {
                HintShape::RoundButton => (f64::from(text_width) + CAP_PAD_X).max(height),
                HintShape::Keycap | HintShape::Gesture => {
                    (f64::from(text_width) + CAP_PAD_X * 2.0).max(MIN_CAP_WIDTH)
                }
            };
            let rect = Rect::new(x, origin.y, x + width, origin.y + height);
            x += width + CAP_GAP;
            (*shape, rect)
        })
        .collect()
}

fn paint_token(ctx: &mut PaintCtx<'_>, rect: Rect, label: &str, shape: HintShape) {
    let theme = ctx.theme;
    let colors = &theme.tokens.colors;
    let radius = match shape {
        HintShape::RoundButton => rect.height() / 2.0,
        HintShape::Gesture => 11.0,
        HintShape::Keycap => 5.0,
    };
    // Keycap body is a lift off the ground with an edge derived from it
    // (HIG §9.2, §9.5) — ramp slots were one fixed grey per theme.
    crate::surface::card(ctx.scene, ctx.transform, rect, radius, colors.lift(1.0));
    let style = token_text_style(theme);
    let (tw, th) = ctx.text.measure_style(label, None, &style);
    ctx.text(
        label,
        &style.clone().with_weight(TextWeight::Semibold),
        colors.text_primary,
        Point::new(
            rect.center().x - f64::from(tw) / 2.0,
            rect.center().y - f64::from(th) / 2.0,
        ),
        None,
    );
}

fn token_text_style(theme: &Theme) -> TextStyle {
    theme.text(TextRole::Caption)
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

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.layout_theme = theme.clone();
        let height = token_height(theme);
        let approximate = self
            .tokens()
            .iter()
            .map(|(label, shape)| match shape {
                HintShape::RoundButton => {
                    (crate::num::f64_of_usize(label.chars().count()) * 7.0 + CAP_PAD_X).max(height)
                }
                HintShape::Keycap | HintShape::Gesture => {
                    (crate::num::f64_of_usize(label.chars().count()) * 7.0 + CAP_PAD_X * 2.0)
                        .max(MIN_CAP_WIDTH)
                }
            })
            .sum::<f64>()
            + CAP_GAP * crate::num::f64_of_usize(self.tokens().len().saturating_sub(1));
        let style = Style {
            size: Size {
                width: Dimension::length(crate::num::f32_of_f64(approximate)),
                height: Dimension::length(crate::num::f32_of_f64(
                    f64::from(theme.tokens.spacing.s6).max(height),
                )),
            },
            // Fixed target: never the item that gives in an overflowing row.
            flex_shrink: 0.0,
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn measure(
        &mut self,
        _known: Size<Option<f32>>,
        _available: Size<taffy::AvailableSpace>,
        text_renderer: &mut TextRenderer,
    ) -> Size<f32> {
        Size {
            width: crate::num::f32_of_f64(Self::measure_width(
                &self.binding,
                self.conventions(),
                text_renderer,
                &self.layout_theme,
            )),
            height: crate::num::f32_of_f64(token_height(&self.layout_theme)),
        }
    }

    /// The estimate `build_layout` reserved becomes the keycaps' measured
    /// width, so the hint ends where its last keycap does and a column of
    /// hints shares one trailing edge.
    fn adjust_layout_style(
        &mut self,
        tree: &mut TaffyTree<()>,
        node: NodeId,
        text: &mut TextRenderer,
    ) -> bool {
        let width =
            Self::measure_width(&self.binding, self.conventions(), text, &self.layout_theme).ceil();
        let Ok(current) = tree.style(node) else {
            return false;
        };
        let wanted = Dimension::length(crate::num::f32_of_f64(width));
        if current.size.width == wanted {
            return false;
        }
        let mut style = current.clone();
        style.size.width = wanted;
        tree.set_style(node, style).is_ok()
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
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
            &mut PaintCtx::new(scene, transform, text_renderer, theme),
            Point::new(
                self.rect.x0,
                self.rect.center().y - token_height(theme) / 2.0,
            ),
            &self.binding,
            self.conventions(),
        );
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
    use taffy::prelude::TaffyMaxContent;

    #[test]
    fn input_hint_scales_layout_and_names_platform_legends() {
        fn retained_layout_grows_with_large_text() {
            let mut hint = InputHint::keyboard(KeyboardBinding::new([], KeyLegend::Enter));
            let mut taffy = TaffyTree::new();
            let mut map = HashMap::new();
            let theme = Theme::dark().with_text_scale(2.0);
            hint.build_layout(&mut taffy, &theme, &mut map);
            let mut text = TextRenderer::new();
            let measured = hint.measure(
                Size {
                    width: None,
                    height: None,
                },
                Size::MAX_CONTENT,
                &mut text,
            );
            assert!(measured.height > crate::num::f32_of_f64(HEIGHT));
            assert_eq!(hint.layout_theme.text_scale, 2.0);
        }

        fn hint_accessibility_uses_platform_and_controller_legends() {
            let mut keyboard = InputHint::keyboard(KeyboardBinding::new(
                [KeyboardModifier::Command, KeyboardModifier::Shift],
                KeyLegend::Enter,
            ));
            keyboard.set_platform_conventions(PlatformConventions::new(Platform::MacOs));
            assert_eq!(keyboard.accessible_label(), "⌘ + ⇧ + ↩");

            let mut controller =
                InputHint::new(InputBinding::Controller(ControllerBinding::new([
                    ControllerButton::South,
                ])));
            controller.set_interaction_environment(
                InteractionEnvironment::POINTER.with_controller(ControllerFamily::Nintendo),
            );
            assert_eq!(controller.accessible_label(), "B");
        }

        retained_layout_grows_with_large_text();
        hint_accessibility_uses_platform_and_controller_legends();
    }
}
