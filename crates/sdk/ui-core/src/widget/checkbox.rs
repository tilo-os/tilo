use super::bool_control::{
    BoolControl, bool_control_api, bool_control_capabilities, bool_control_widget_methods,
};
use crate::text_render::TextRenderer;
use crate::text_render::TextStyle;
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Point, Rect, RoundedRect, Stroke};

/// The box itself: 24 px (`spacing.s6` at 1×), centred in the interaction
/// minimum target so the tap area stays 32/48 px while the mark reads at
/// label size. Fixed rather than read from the scaled spacing tokens because
/// the target it sits in does not follow `ui_scale` either.
const BOX_EDGE: f64 = 24.0;
/// Space between the target's trailing edge and the label; shared by
/// `measure` and `draw` so the label lands where it was measured. 10 px is
/// deliberately between `s2` and `s3`: the 24 px box already leaves 4 px of
/// its 32 px target empty on that side.
const LABEL_GAP: f32 = 10.0;

/// Interaction state for an immediate-mode checkbox face. Canvas apps keep
/// their own hit-testing while sharing the retained checkbox's visual states.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct CheckboxInteract {
    pub hovered: bool,
    pub focused: bool,
    pub disabled: bool,
}

/// Stateless checkbox presentation for hand-laid-out canvases.
///
/// [`Checkbox`] remains the right choice when the value is bound directly to
/// an Automerge path. `CheckboxFace` avoids every immediate-mode app copying
/// the box, focus border, disabled treatment, and proportional check mark.
pub struct CheckboxFace {
    rect: Rect,
    checked: bool,
    interact: CheckboxInteract,
}

impl CheckboxFace {
    pub fn new(rect: Rect, checked: bool) -> Self {
        Self {
            rect,
            checked,
            interact: CheckboxInteract::default(),
        }
    }

    #[must_use]
    pub fn hovered(mut self, hovered: bool) -> Self {
        self.interact.hovered = hovered;
        self
    }

    #[must_use]
    pub fn focused(mut self, focused: bool) -> Self {
        self.interact.focused = focused;
        self
    }

    #[must_use]
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.interact.disabled = disabled;
        self
    }
}

pub struct Checkbox {
    control: BoolControl,
    /// Optional visible label owned by the checkbox itself. Unlike a sibling
    /// `Text`, this makes the entire rendered row one pointer, keyboard and
    /// accessibility target.
    label: Option<String>,
    label_style: TextStyle,
    /// A labelled box as wide as its content instead of its row.
    intrinsic: bool,
    /// What the last presented frame painted, and what the frame being drawn
    /// paints: the checkbox needs a redraw only when they differ.
    painted: Option<Painted>,
    drawing: Option<Painted>,
}

/// What a checkbox's paint depends on besides its place and the theme:
/// checked, hovered, focused, enabled, and the catalogue its label reads.
type Painted = (bool, bool, bool, bool, u64);

impl Checkbox {
    pub fn new(_id: &str, on_toggle_path: &str) -> Self {
        Self::with_control(BoolControl::new(on_toggle_path, None))
    }

    /// Create a true two-way binding to typed process-local boolean state.
    ///
    /// Pointer, keyboard, and accessibility activation update `state`;
    /// mutations through any clone of `state` update this checkbox on the
    /// next synchronization pass. Use [`Self::on_change`] only to observe
    /// user changes, not to keep the model in sync.
    pub fn from_state(state: &crate::LocalState<bool>) -> Self {
        Self::with_control(BoolControl::local(state, None))
    }

    fn with_control(control: BoolControl) -> Self {
        Self {
            control,
            label: None,
            label_style: crate::tokens::typography::stock().style(crate::tokens::TextRole::Body),
            intrinsic: false,
            painted: None,
            drawing: None,
        }
    }

    fn painted_now(&self) -> Painted {
        (
            self.control.is_checked,
            self.control.is_hovered,
            self.control.is_focused,
            self.control.effectively_enabled(),
            crate::i18n::I18nManager::revision(),
        )
    }

    bool_control_api!();

    /// A labelled checkbox as wide as its box and label, instead of the row:
    /// it sits beside other controls in a row.
    #[must_use]
    pub fn intrinsic_width(mut self) -> Self {
        self.intrinsic = true;
        self
    }

    /// Add an inline visible label and make its full row activate this
    /// checkbox. Use this instead of a sibling `Text` when a form sentence is
    /// meant to be one large click target.
    #[must_use]
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        let label = label.into();
        self.control.accessible_name = Some(label.clone());
        self.label = Some(label);
        self
    }

    /// Paint a checkbox without taking ownership of its state or events.
    ///
    /// The box is always square: the largest square centred in `face.rect`,
    /// with the item radius.
    pub fn paint(scene: &mut Scene, transform: Affine, theme: &Theme, face: &CheckboxFace) {
        let rect = square_box(face.rect);
        let radius = f64::from(theme.tokens.radii.item);
        let shape = RoundedRect::from_rect(rect, radius);
        if face.interact.disabled {
            scene.push_layer(
                vello::peniko::Fill::NonZero,
                vello::peniko::BlendMode::default(),
                crate::widget::DISABLED_ALPHA,
                transform,
                &shape,
            );
        }

        // HIG §9.5: unchecked is the raised control surface with an edge
        // derived from it, and hover is that surface's lift; a declared
        // `border_standard` vanished on a raised dark card.
        let box_color = if face.checked {
            theme.tokens.colors.intent.inverted
        } else if face.interact.hovered {
            theme
                .tokens
                .colors
                .lift_on(theme.tokens.colors.surfaces.raised, 0.5)
        } else {
            theme.tokens.colors.surfaces.raised
        };
        {
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                box_color,
                None,
                &shape,
            );
        }

        if face.checked {
            let mut path = BezPath::new();
            path.move_to(Point::new(
                rect.x0 + rect.width() * 0.25,
                rect.y0 + rect.height() * 0.50,
            ));
            path.line_to(Point::new(
                rect.x0 + rect.width() * 0.42,
                rect.y0 + rect.height() * 0.67,
            ));
            path.line_to(Point::new(
                rect.x0 + rect.width() * 0.75,
                rect.y0 + rect.height() * 0.25,
            ));
            scene.stroke(
                &Stroke::new((rect.width().min(rect.height()) * 0.105).max(1.5))
                    .with_caps(vello::kurbo::Cap::Round)
                    .with_join(vello::kurbo::Join::Round),
                transform,
                theme.tokens.colors.ink_on_any(
                    &[box_color],
                    theme.tokens.colors.intent.on_inverted,
                    crate::tokens::colors::NON_TEXT_MIN_CONTRAST,
                ),
                None,
                &path,
            );
        }

        if face.interact.disabled {
            scene.pop_layer();
        }

        // Focus ring wins; the accent-filled checked box is self-defining
        // (like a Primary button) and takes no rest edge. The edge is drawn
        // outside the disabled layer: a disabled box keeps its bounds at full
        // strength (HIG §9.5), or an unchecked one vanishes on a light ground.
        // The unchecked edge is the only thing that shows the box on its
        // ground, so it is held to the 3:1 a control's bounds need there.
        let edge = if face.interact.focused {
            Some((2.0, theme.tokens.colors.intent.you))
        } else if face.checked {
            None
        } else {
            Some((1.0, unchecked_edge(theme, box_color)))
        };
        if let Some((width, color)) = edge {
            // Rest edge and focus ring share one half-pixel-inset outline.
            scene.stroke(
                &Stroke::new(width),
                transform,
                color,
                None,
                &crate::surface::edge_outline(rect, radius),
            );
        }
    }
}

/// The edge of an unchecked box: derived from its face and resolved until
/// it reaches the non-text contrast floor (3:1) against the ground the box
/// stands on. The derived edge is a translucent hairline, so it is measured
/// as painted — over the face it is stroked on — never as its opaque base.
fn unchecked_edge(theme: &Theme, face: vello::peniko::Color) -> vello::peniko::Color {
    let colors = &theme.tokens.colors;
    colors.ink_on_any(
        &[colors.surfaces.content],
        crate::tokens::colors::composite(crate::surface::edge_on(face), face),
        crate::tokens::colors::NON_TEXT_MIN_CONTRAST,
    )
}

impl Widget for Checkbox {
    bool_control_widget_methods!("checkbox");

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.label_style = theme.tokens.typography.style(crate::tokens::TextRole::Body);
        let labelled = self.label.is_some();
        let style = Style {
            size: Size {
                width: if labelled && !self.intrinsic {
                    Dimension::percent(1.0)
                } else if labelled {
                    Dimension::auto()
                } else {
                    Dimension::length(self.control.interaction.minimum_target())
                },
                height: if labelled {
                    Dimension::auto()
                } else {
                    Dimension::length(self.control.interaction.minimum_target())
                },
            },
            min_size: Size::from_lengths(
                self.control.interaction.minimum_target(),
                self.control.interaction.minimum_target(),
            ),
            flex_shrink: 1.0,
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn measure(
        &mut self,
        known_dimensions: Size<Option<f32>>,
        available_space: Size<taffy::AvailableSpace>,
        text_renderer: &mut TextRenderer,
    ) -> Size<f32> {
        let target = self.control.interaction.minimum_target();
        let Some(label) = self.label.as_ref() else {
            return Size {
                width: known_dimensions.width.unwrap_or(target),
                height: known_dimensions.height.unwrap_or(target),
            };
        };
        let gap = LABEL_GAP;
        let available_width = known_dimensions.width.or(match available_space.width {
            taffy::AvailableSpace::Definite(width) => Some(width),
            taffy::AvailableSpace::MinContent => Some(target + gap),
            taffy::AvailableSpace::MaxContent => None,
        });
        let label_limit = available_width.map(|width| (width - target - gap).max(0.0));
        let (label_width, label_height) =
            text_renderer.measure_style(&crate::i18n!(label), label_limit, &self.label_style);
        // An intrinsic box is as wide as its label needs, within the room.
        let hugged = self.intrinsic.then(|| {
            let (natural, _) =
                text_renderer.measure_style(&crate::i18n!(label), None, &self.label_style);
            let natural = target + gap + natural.ceil();
            available_width.map_or(natural, |room| natural.min(room))
        });
        Size {
            width: known_dimensions
                .width
                .or(hugged)
                .or(available_width)
                .unwrap_or(target + gap + label_width),
            height: known_dimensions
                .height
                .unwrap_or_else(|| label_height.max(target)),
        }
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.control.rect = crate::widget::layout_rect(taffy, node, origin);
    }

    #[allow(
        clippy::cast_possible_truncation,
        reason = "widget rectangles are finite renderer coordinates already laid out as f32"
    )]
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &crate::presence::PresenceCtx,
    ) {
        self.drawing = Some(self.painted_now());
        let target = f64::from(self.control.interaction.minimum_target());
        let visual_center = if self.label.is_some() {
            Point::new(
                self.control.rect.x0 + target / 2.0,
                self.control.rect.center().y,
            )
        } else {
            self.control.rect.center()
        };
        let visual = Rect::from_center_size(visual_center, (BOX_EDGE, BOX_EDGE));
        Self::paint(
            scene,
            transform,
            theme,
            &CheckboxFace::new(visual, self.control.is_checked)
                .hovered(self.control.is_hovered)
                .focused(self.control.is_focused)
                .disabled(!self.control.effectively_enabled()),
        );
        if let Some(label) = self.label.as_ref() {
            let label = crate::i18n!(label);
            let gap = f64::from(LABEL_GAP);
            let x = self.control.rect.x0 + target + gap;
            let max_width = (self.control.rect.x1 - x).max(0.0);
            let style = self.label_style.clone();
            let (_, label_height) =
                text_renderer.measure_style(&label, Some(max_width as f32), &style);
            let y = self.control.rect.y0
                + ((self.control.rect.height() - f64::from(label_height)) / 2.0).max(0.0);
            text_renderer.draw_style(
                scene,
                &label,
                theme.tokens.colors.text_primary,
                transform.pre_translate((x, y).into()),
                Some(max_width as f32),
                &style,
            );
        }
        crate::widget::draw_gate_and_peer_focus(self, scene, transform, theme, presence);
    }

    fn name(&self) -> &'static str {
        "Checkbox"
    }

    /// A checkbox repaints when what it paints changed since the frame on
    /// screen, not on every frame: a long list of them does not repaint
    /// whole for a scroll or a keystroke elsewhere.
    fn needs_redraw(&self) -> bool {
        self.painted != Some(self.painted_now())
    }

    fn mark_drawn(&mut self) {
        if let Some(drawing) = self.drawing.take() {
            self.painted = Some(drawing);
        }
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        // A visible inline label names itself; an explicit accessible name
        // covers label-less uses so the control is never announced as an
        // anonymous checkbox. The checked state is the load-bearing semantic.
        let name = self
            .control
            .accessible_name
            .as_ref()
            .map(|n| crate::i18n!(n))
            .filter(|n| !n.is_empty());
        let mut node = AccessNode::new(Role::Checkbox)
            .with_semantic_key(self.test_id().unwrap_or("checkbox"))
            .with_optional_name(name)
            .with_state(
                AccessState::new()
                    .focusable(self.is_focusable())
                    .focused(self.is_focused())
                    .disabled(!self.is_enabled())
                    .checked(self.control.is_checked),
            );
        if self.control.action.is_none() && self.control.on_change.is_none() {
            node = node.with_action_target(self.test_id().unwrap_or("checkbox"));
        }
        Some(node)
    }
}

bool_control_capabilities!(Checkbox, "checkbox");

/// The largest square centred in `rect`: a checkbox is always square,
/// whatever rect a canvas hands its face.
fn square_box(rect: Rect) -> Rect {
    let side = rect.width().min(rect.height());
    Rect::from_center_size(rect.center(), (side, side))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Dispatch;
    use crate::widget::Widget;
    use automerge::{AutoCommit, ReadDoc, transaction::Transactable};
    use winit::event::{ElementState, MouseButton, WindowEvent};

    #[test]
    fn an_unchecked_box_is_square_in_any_rect() {
        for rect in [
            Rect::new(0.0, 0.0, 40.0, 20.0),
            Rect::new(10.0, 10.0, 30.0, 60.0),
            Rect::new(0.0, 0.0, 24.0, 24.0),
        ] {
            let square = square_box(rect);
            assert_eq!(square.width(), square.height(), "{rect:?}");
            assert_eq!(square.center(), rect.center());
            assert!(rect.contains(square.origin()));
        }
    }

    /// The unchecked edge, as painted, holds 3:1 against every region's
    /// ground in every preset: it is the only thing that shows the box.
    #[test]
    fn an_unchecked_edge_reads_on_every_ground() {
        use crate::tokens::colors::{NON_TEXT_MIN_CONTRAST, composite};
        for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
            let s = &theme.tokens.colors.surfaces;
            for ground in [s.content, s.sidebar, s.inspector, s.raised, s.overlay] {
                let mut region = theme.clone();
                region.tokens.colors.surfaces.content = ground;
                let face = region.tokens.colors.surfaces.raised;
                let edge = composite(unchecked_edge(&region, face), face);
                let ratio = crate::a11y::contrast_ratio(edge, ground);
                assert!(
                    ratio >= NON_TEXT_MIN_CONTRAST - 0.01,
                    "{}: the unchecked edge reaches only {ratio:.2}:1 on {ground:?}",
                    theme.name
                );
            }
        }
    }

    /// A disabled box dims its face but keeps its edge at full strength
    /// (HIG §9.5): an unchecked white box on a light ground would otherwise
    /// vanish.
    #[test]
    fn a_disabled_box_keeps_its_edge_at_full_strength() {
        use crate::offscreen::OffscreenRenderer;
        let theme = Theme::light();
        let rect = Rect::new(8.0, 8.0, 32.0, 32.0);
        let render = |disabled: bool| {
            let mut scene = Scene::new();
            Checkbox::paint(
                &mut scene,
                Affine::IDENTITY,
                &theme,
                &CheckboxFace::new(rect, false).disabled(disabled),
            );
            OffscreenRenderer::new(40, 40).render(&scene, theme.tokens.colors.surfaces.content)
        };
        let (rest, disabled) = (render(false), render(true));
        // The outermost pixel column of the box is its edge.
        let rest_edge = OffscreenRenderer::sample(&rest, 40, 8, 20);
        let disabled_edge = OffscreenRenderer::sample(&disabled, 40, 8, 20);
        let ground = OffscreenRenderer::sample(&rest, 40, 2, 20);
        let delta = |a: (u8, u8, u8, u8), b: (u8, u8, u8, u8)| {
            a.0.abs_diff(b.0)
                .max(a.1.abs_diff(b.1))
                .max(a.2.abs_diff(b.2))
        };
        assert!(
            delta(rest_edge, disabled_edge) <= 4,
            "disabled edge {disabled_edge:?} vs rest edge {rest_edge:?}"
        );
        assert!(delta(disabled_edge, ground) >= delta(rest_edge, ground).saturating_sub(4));
    }

    #[test]
    fn checkbox_identity_face_and_scaled_label_geometry() {
        fn checkbox_test_id_default_is_checkbox() {
            let c = Checkbox::new("any", "path");
            assert_eq!(Widget::test_id(&c), Some("checkbox"));
        }

        fn checkbox_test_id_can_be_overridden() {
            let c = Checkbox::new("any", "path").with_id("remember_me");
            assert_eq!(Widget::test_id(&c), Some("remember_me"));
        }

        fn immediate_face_preserves_canvas_owned_state() {
            let rect = Rect::new(2.0, 3.0, 18.0, 19.0);
            let face = CheckboxFace::new(rect, true)
                .hovered(true)
                .focused(true)
                .disabled(true);
            assert_eq!(face.rect, rect);
            assert!(face.checked);
            assert!(face.interact.hovered);
            assert!(face.interact.focused);
            assert!(face.interact.disabled);
        }

        fn visible_label_is_part_of_the_checkbox_click_target() {
            let mut checkbox = Checkbox::new("ack", "acknowledged")
                .with_id("acknowledgement")
                .with_label("I understand the consequence.");
            let mut tree = TaffyTree::<()>::new();
            let mut node_map = std::collections::HashMap::new();
            let theme = Theme::dark();
            let node = checkbox.build_layout(&mut tree, &theme, &mut node_map);
            let mut text = TextRenderer::new();
            tree.compute_layout_with_measure(
                node,
                Size {
                    width: taffy::AvailableSpace::Definite(360.0),
                    height: taffy::AvailableSpace::Definite(100.0),
                },
                |known, available, node_id, _, _| {
                    node_map.get(&node_id).map_or(Size::ZERO, |&ptr| unsafe {
                        (*ptr).measure(known, available, &mut text)
                    })
                },
            )
            .expect("labelled checkbox layout");
            checkbox.apply_layout(&tree, node, Point::ZERO);
            let row_rect = checkbox.control.rect;
            assert!(row_rect.width() > 300.0, "row={row_rect:?}");

            let device = winit::event::DeviceId::dummy();
            let mut sender = Dispatch::new().layout(&tree, node);
            let moved = sender.send(
                &mut checkbox,
                &WindowEvent::CursorMoved {
                    device_id: device,
                    position: (320.0, row_rect.center().y).into(),
                },
            );
            let pressed = sender.send(
                &mut checkbox,
                &WindowEvent::MouseInput {
                    device_id: device,
                    state: ElementState::Pressed,
                    button: MouseButton::Left,
                },
            );
            let focus = pressed.focus_target.or(moved.focus_target);

            assert!(checkbox.is_checked());
            assert_eq!(focus, Some(node));
            assert_eq!(
                checkbox.accessibility().and_then(|node| node.name),
                Some("I understand the consequence.".into())
            );
        }

        fn labelled_checkbox_applies_theme_text_scale_once_in_measure_and_draw() {
            let mut checkbox = Checkbox::new("updates", "updates").with_label("Updates");
            for (theme_scale, renderer_scale) in [(2.0, 1.0), (2.0, 2.0), (1.0, 2.0), (2.0, 1.0)] {
                let theme = Theme::dark().with_text_scale(theme_scale);
                let mut tree = TaffyTree::new();
                let mut map = std::collections::HashMap::new();
                checkbox.build_layout(&mut tree, &theme, &mut map);
                let mut renderer = crate::font_catalog::fixture_renderer();
                renderer.set_text_scale(renderer_scale);
                let measured = checkbox.measure(
                    Size {
                        width: None,
                        height: None,
                    },
                    Size {
                        width: taffy::AvailableSpace::MaxContent,
                        height: taffy::AvailableSpace::MaxContent,
                    },
                    &mut renderer,
                );
                let expected = theme
                    .text(crate::tokens::TextRole::Body)
                    .line_height_px()
                    .max(checkbox.control.interaction.minimum_target());
                assert!(
                    (measured.height - expected).abs() < 0.1,
                    "theme{theme_scale} renderer{renderer_scale}: {measured:?}"
                );
                checkbox.control.rect = Rect::new(
                    0.0,
                    0.0,
                    f64::from(measured.width),
                    f64::from(measured.height),
                );
                let mut scene = Scene::new();
                checkbox.draw(
                    &mut scene,
                    Affine::IDENTITY,
                    &mut renderer,
                    &theme,
                    &crate::presence::PresenceCtx::default(),
                );
                assert!(!scene.encoding().resources.glyph_runs.is_empty());
                for run in &scene.encoding().resources.glyph_runs {
                    assert!((run.font_size - 14.0 * theme_scale).abs() < 0.1);
                }
            }
        }

        checkbox_test_id_default_is_checkbox();
        checkbox_test_id_can_be_overridden();
        immediate_face_preserves_canvas_owned_state();
        visible_label_is_part_of_the_checkbox_click_target();
        labelled_checkbox_applies_theme_text_scale_once_in_measure_and_draw();
    }

    #[test]
    fn checkbox_accessibility_and_actions_respect_enabled_state() {
        fn with_enabled_false_is_reported_by_is_enabled() {
            let c = Checkbox::new("any", "path").with_enabled(false);
            assert!(!Widget::is_enabled(&c));
            assert!(
                Widget::is_enabled(&Checkbox::new("any", "path")),
                "default is enabled"
            );
        }

        fn accessibility_unchecked_default() {
            use crate::a11y::Role;
            let c = Checkbox::new("any", "path");
            let node = Widget::accessibility(&c).unwrap();
            assert_eq!(node.role, Role::Checkbox);
            assert_eq!(node.state.checked, Some(false));
            assert!(!node.state.disabled);
            assert!(node.state.focusable);
        }

        fn accessibility_uses_accessible_name_when_set() {
            // No name by default (the label is usually a sibling); the explicit
            // accessible name names a checkbox that stands alone.
            let plain = Checkbox::new("any", "path");
            assert_eq!(Widget::accessibility(&plain).unwrap().name, None);

            let named = Checkbox::new("any", "path").with_accessible_name("Remember me");
            assert_eq!(
                Widget::accessibility(&named).unwrap().name.as_deref(),
                Some("Remember me"),
            );
        }

        fn accessibility_disabled_checked_reports_checked_and_disabled() {
            use crate::a11y::Role;
            let mut c = Checkbox::new("any", "path").with_enabled(false);
            c.control.is_checked = true; // same module → can set the synced state directly
            let node = Widget::accessibility(&c).unwrap();
            assert_eq!(node.role, Role::Checkbox);
            assert_eq!(node.state.checked, Some(true), "checked state surfaces");
            assert!(node.state.disabled, "disabled surfaces");
        }

        fn perform_action_toggle_check_flips_state() {
            use crate::a11y::Action;
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            let mut c = Checkbox::new("any", "checked");
            assert!(!c.control.is_checked);
            assert!(Widget::perform_action(
                &mut c,
                &Action::ToggleCheck,
                &[],
                &mut crate::widget::Edits::lend(&mut doc),
                &root
            ));
            assert!(c.control.is_checked, "ToggleCheck flips to checked");
            // Activate also toggles.
            assert!(Widget::perform_action(
                &mut c,
                &Action::Activate,
                &[],
                &mut crate::widget::Edits::lend(&mut doc),
                &root
            ));
            assert!(!c.control.is_checked, "Activate flips back");
        }

        fn disabled_checkbox_rejects_perform_action() {
            use crate::a11y::Action;
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            let mut c = Checkbox::new("any", "checked").with_enabled(false);
            assert!(!Widget::perform_action(
                &mut c,
                &Action::ToggleCheck,
                &[],
                &mut crate::widget::Edits::lend(&mut doc),
                &root
            ));
            assert!(!c.control.is_checked, "a disabled checkbox must not toggle");
        }

        fn disabled_checkbox_does_not_toggle() {
            let mut c = Checkbox::new("id", "checked").with_enabled(false);

            let mut tree = TaffyTree::<()>::new();
            let mut node_map = std::collections::HashMap::new();
            let theme = Theme::dark();
            let node = c.build_layout(&mut tree, &theme, &mut node_map);
            tree.compute_layout(
                node,
                taffy::Size {
                    width: taffy::AvailableSpace::Definite(50.0),
                    height: taffy::AvailableSpace::Definite(50.0),
                },
            )
            .unwrap();
            c.apply_layout(&tree, node, Point::ZERO);

            let device = winit::event::DeviceId::dummy();
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            let mut sender = Dispatch::new().doc(&mut doc).layout(&tree, node);
            let moved = sender.send(
                &mut c,
                &WindowEvent::CursorMoved {
                    device_id: device,
                    position: (10.0, 10.0).into(),
                },
            );
            let pressed = sender.send(
                &mut c,
                &WindowEvent::MouseInput {
                    device_id: device,
                    state: ElementState::Pressed,
                    button: MouseButton::Left,
                },
            );
            let focus = pressed.focus_target.or(moved.focus_target);
            let handled = moved.handled || pressed.handled;

            assert!(focus.is_none(), "disabled checkbox must not request focus");
            assert!(!handled, "disabled checkbox must not swallow input");
            assert!(
                doc.get(&root, "checked").unwrap().is_none(),
                "disabled checkbox must not write the CRDT path"
            );
        }

        fn action_binding_owns_accessibility_activation() {
            let mut checkbox = Checkbox::new("native", "legacy")
                .on_change(|_| panic!("legacy callback must be suppressed"))
                .on_action(crate::widget::test_bool_action_binding());
            let mut doc = AutoCommit::new();
            crate::widget::controls::refresh_control_action_availability(
                &mut checkbox,
                &std::collections::HashSet::new(),
            );
            assert!(!Widget::is_enabled(&checkbox));
            assert!(!Widget::perform_action(
                &mut checkbox,
                &crate::a11y::Action::Activate,
                &[],
                &mut crate::widget::Edits::lend(&mut doc),
                &automerge::ROOT
            ));
            crate::widget::controls::refresh_control_action_availability(
                &mut checkbox,
                &std::collections::HashSet::from([tilo_actions::ActionAddress::new(
                    "test:control",
                    "set",
                )]),
            );
            assert!(Widget::is_enabled(&checkbox));
            assert!(Widget::perform_action(
                &mut checkbox,
                &crate::a11y::Action::Activate,
                &[],
                &mut crate::widget::Edits::lend(&mut doc),
                &automerge::ROOT
            ));
            assert!(doc.get(&automerge::ROOT, "legacy").unwrap().is_none());
            let intent = crate::widget::controls::take_control_invocation(&mut checkbox).unwrap();
            assert_eq!(intent.arguments["value"], true);
            assert!(matches!(
                crate::widget::controls::control_action_bindings(&checkbox)[0].kind,
                crate::widget::ControlBindingKind::Action(_)
            ));
            doc.put(&automerge::ROOT, "legacy", false).unwrap();
            Widget::sync(&mut checkbox, &doc, &automerge::ROOT);
            assert!(
                !checkbox.is_checked(),
                "the document path remains the read model"
            );
        }

        with_enabled_false_is_reported_by_is_enabled();
        accessibility_unchecked_default();
        accessibility_uses_accessible_name_when_set();
        accessibility_disabled_checked_reports_checked_and_disabled();
        perform_action_toggle_check_flips_state();
        disabled_checkbox_rejects_perform_action();
        disabled_checkbox_does_not_toggle();
        action_binding_owns_accessibility_activation();
    }

    #[test]
    fn checkbox_bindings_follow_document_and_local_state() {
        fn document_binding_resets_after_the_boolean_path_disappears() {
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            let mut checkbox = Checkbox::new("any", "checked");
            doc.put(&root, "checked", true).unwrap();
            Widget::sync(&mut checkbox, &doc, &root);
            assert!(checkbox.is_checked());

            doc.delete(&root, "checked").unwrap();
            Widget::sync(&mut checkbox, &doc, &root);
            assert!(!checkbox.is_checked(), "a removed path means unchecked");

            doc.put(&root, "checked", "not-a-boolean").unwrap();
            Widget::sync(&mut checkbox, &doc, &root);
            assert!(!checkbox.is_checked(), "a mistyped path cannot stay stale");
        }

        fn local_state_binding_is_two_way_across_checkboxes() {
            use crate::a11y::Action;

            let state = crate::LocalState::new(false);
            let changes = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
            let captured = std::rc::Rc::clone(&changes);
            let mut first = Checkbox::from_state(&state)
                .on_change(move |checked| captured.borrow_mut().push(checked));
            let mut second = Checkbox::from_state(&state);
            let mut document = AutoCommit::new();

            state.set(true);
            Widget::sync(&mut first, &document, &automerge::ROOT);
            Widget::sync(&mut second, &document, &automerge::ROOT);
            assert!(first.is_checked());
            assert!(second.is_checked());
            assert!(changes.borrow().is_empty(), "model sync is not user intent");

            assert!(Widget::perform_action(
                &mut first,
                &Action::ToggleCheck,
                &[],
                &mut crate::widget::Edits::lend(&mut document),
                &automerge::ROOT,
            ));
            assert!(!state.get(), "user activation publishes typed state");
            assert_eq!(&*changes.borrow(), &[false]);

            Widget::sync(&mut second, &document, &automerge::ROOT);
            assert!(!second.is_checked(), "sibling follows the shared model");

            second.set_checked(true);
            assert!(state.get(), "programmatic mutation publishes typed state");
            Widget::sync(&mut first, &document, &automerge::ROOT);
            assert!(first.is_checked());
            assert_eq!(
                &*changes.borrow(),
                &[false],
                "programmatic mutation does not invoke the user callback"
            );
        }

        document_binding_resets_after_the_boolean_path_disappears();
        local_state_binding_is_two_way_across_checkboxes();
    }
    #[test]
    fn unchecked_edge_reaches_three_to_one_and_a_labelled_box_can_hug() {
        for theme in [Theme::dark(), Theme::light()] {
            let face = theme.tokens.colors.surfaces.raised;
            let edge = unchecked_edge(&theme, face);
            let ratio = crate::a11y::contrast_ratio(edge, theme.tokens.colors.surfaces.content);
            assert!(ratio >= 3.0 - 1e-6, "edge contrast {ratio}");
        }
        let row = crate::widget::Row::new().push(Box::new(
            Checkbox::new("c", "c")
                .with_id("hug")
                .with_label("Remember me")
                .intrinsic_width(),
        ));
        let app =
            crate::headless::FrameHarness::new(crate::State::new(), Box::new(row), (900, 200));
        let rect = *app.targets().get("hug").expect("checkbox");
        assert!(
            rect.width() < 300.0,
            "a hugging box is as wide as its label: {rect:?}"
        );
    }
}
