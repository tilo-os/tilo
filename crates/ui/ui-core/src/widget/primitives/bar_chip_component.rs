//! [`BarChip`] as a described component.

use std::collections::HashMap;

use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::kurbo::{Point, Rect};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::widget::primitives::BarChipFace;
use tilo_ui_base::icons::Icon;
use tilo_ui_base::paint::PaintCtx;
use tilo_ui_base::text_render::TextRenderer;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::{
    component::Component, component::Described, component::Event, component::Person,
};
use tilo_ui_base::{presence::Face, presence::FaceStack};
use tilo_ui_base::{widget::TaffyTreeExt, widget::Widget};

/// A bar control for people: their faces, a label and a press, with faces opening who is here
/// and an invite opening the host's invite flow.
#[derive(Component)]
#[component(name = "BarChip", role = "Button", label = "name")]
pub struct BarChipProps {
    /// The chip's name for assistive technology, and its tooltip when it shows only a glyph.
    #[component(required, positional)]
    pub name: String,
    /// The words beside the faces: "Editing", "Invite".
    pub label: Option<String>,
    /// A glyph before the faces: a watch, an invitation.
    pub icon: Option<Icon>,
    /// A trailing glyph: the chevron of a chip that opens a panel.
    pub trailing: Option<Icon>,
    /// Who it shows, in order: their faces, then a count for everyone past them.
    pub who: Vec<Person>,
    /// The chip's panel is open: the chip is lifted as a whole.
    pub open: bool,
    /// Keyboard focus; the ring shows only while focus is visible.
    pub focused: bool,
    /// The most the label may grow to before it elides.
    pub label_width: Option<f64>,
    /// A person whose presence colour washes the chip's body: who the state it says is about.
    pub tone: Option<Person>,
    /// Whether it can be pressed now.
    pub enabled: bool,
    /// Why it cannot be pressed now, where the name does not say (an action whose requirement
    /// does not hold): spoken as its description beside the name, and shown on hover.
    pub description: Option<String>,
    /// A press: the chip's action.
    #[component(event(press))]
    pub press: Event,
}

/// A chip is pressable until something says otherwise.
impl Default for BarChipProps {
    fn default() -> Self {
        Self {
            name: String::new(),
            label: None,
            icon: None,
            trailing: None,
            who: Vec::new(),
            open: false,
            focused: false,
            label_width: None,
            tone: None,
            enabled: true,
            description: None,
            press: Event::None,
        }
    }
}

impl BarChipProps {
    fn faces(&self, theme: &Theme) -> Vec<Face<'_>> {
        let palette = &theme.tokens.colors.presence;
        self.who
            .iter()
            .map(|who| Face::of(who, palette.hue(who.hue)))
            .collect()
    }
}

/// A bar chip as a retained widget: the [`BarChipFace`]'s content with its own press.
pub struct BarChip {
    props: BarChipProps,
    /// Pointer precision; a coarse pointer grows the face to the minimum
    /// target, as every other control does.
    interaction: tilo_ui_base::interaction::InteractionEnvironment,
    icon_run: Vec<Icon>,
    press: Option<Box<dyn FnMut()>>,
    rect: Rect,
    hovered: bool,
    pressed: bool,
    focused: bool,
    /// Its width is measured from its faces and its label, so a change to either asks the solver
    /// for its box again.
    layout_dirty: bool,
}

impl BarChip {
    /// The chip's face height: the bar's own control height, grown to the
    /// minimum target on a coarse pointer.
    fn face_height(&self, theme: &Theme) -> f64 {
        let base = BarChipFace::height(theme);
        if self.interaction.supports_hover() {
            base
        } else {
            base.max(f64::from(self.interaction.minimum_target()))
        }
    }

    fn face<'a>(&'a self, theme: &Theme, faces: &'a [Face<'a>]) -> BarChipFace<'a> {
        let (shown, overflow) = FaceStack::in_face(BarChipFace::height(theme)).split(faces.len());
        let mut chip = BarChipFace::new(&self.props.name, &faces[..shown.min(faces.len())])
            .overflow(overflow)
            .at_height(self.face_height(theme))
            .open(self.props.open)
            .hovered(self.props.enabled && self.hovered)
            .pressed(self.props.enabled && self.pressed)
            .focused(self.focused);
        if let Some(label) = &self.props.label {
            chip = chip.label(label);
        }
        if !self.icon_run.is_empty() {
            // The glyphs the props name, in the run the face borrows: the
            // face reads glyphs, never a single field.
            chip = chip.icons(&self.icon_run);
        }
        if let Some(trailing) = self.props.trailing {
            chip = chip.trailing(trailing);
        }
        if let Some(width) = self.props.label_width {
            chip = chip.label_width(width);
        }
        if let Some(who) = &self.props.tone {
            chip = chip.tone(tilo_ui_base::presence::identity_colour(who.identity.id()));
        }
        chip
    }

    fn fire_press(&mut self) {
        if !self.props.enabled {
            return;
        }
        if let Some(press) = &mut self.press {
            press();
        }
    }

    /// Whether `point` is on the chip.
    #[must_use]
    pub fn contains(&self, point: Point) -> bool {
        self.rect.contains(point)
    }

    #[must_use]
    pub fn rect(&self) -> Rect {
        self.rect
    }
}

impl Widget for BarChip {
    tilo_ui_base::described!();

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let height = tilo_ui_base::num::f32_of_f64(self.face_height(theme));
        taffy.widget_leaf(
            Style {
                flex_shrink: 0.0,
                size: Size {
                    width: Dimension::auto(),
                    height: Dimension::length(height),
                },
                ..Default::default()
            },
            node_map,
            self,
        )
    }

    fn adjust_layout_style(
        &mut self,
        taffy: &mut TaffyTree<()>,
        node: NodeId,
        text: &mut TextRenderer,
    ) -> bool {
        let theme = Theme::default();
        let faces = self.props.faces(&theme);
        let width = tilo_ui_base::num::f32_of_f64(self.face(&theme, &faces).width(text, &theme));
        let Ok(style) = taffy.style(node) else {
            return false;
        };
        let wanted = Dimension::length(width);
        if style.size.width == wanted {
            return false;
        }
        let mut style = style.clone();
        style.size.width = wanted;
        taffy.set_style(node, style).is_ok()
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = tilo_ui_base::widget::layout_rect(taffy, node, origin);
    }

    /// It measures its faces and its label for its width, so it asks layout until it is placed and
    /// again whenever what it measures changed.
    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO || self.layout_dirty
    }

    fn layout_measured(&mut self) {
        self.layout_dirty = false;
    }

    fn set_interaction_environment(
        &mut self,
        environment: tilo_ui_base::interaction::InteractionEnvironment,
    ) {
        if self.interaction != environment {
            self.interaction = environment;
            self.hovered &= environment.supports_hover();
            // A coarse pointer grows the face to the minimum target.
            self.layout_dirty = true;
        }
    }

    fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text_renderer, theme, mut env) = ctx.parts();
        if self.rect.width() <= 0.0 || self.rect.height() <= 0.0 {
            return;
        }
        let faces = self.props.faces(theme);
        let ground = theme.tokens.colors.surfaces.base;
        // A chip that cannot be pressed now is its rest face under the shared disabled veil.
        let disabled = !self.props.enabled;
        if disabled {
            crate::widget::paint_disabled(scene, transform, &self.rect);
        }
        let _ = self.face(theme, &faces).paint(
            &mut PaintCtx::new(scene, transform, text_renderer, theme),
            self.rect,
            ground,
        );
        if disabled {
            scene.pop_layer();
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut tilo_ui_base::widget::EventCtx<'_>) {
        if !self.props.enabled {
            return;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.hovered = self.rect.contains(ctx.logical(position));
            }
            WindowEvent::CursorLeft { .. } => {
                self.hovered = false;
                self.pressed = false;
            }
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => {
                if !self.hovered {
                    return;
                }
                match state {
                    ElementState::Pressed => {
                        self.pressed = true;
                        *ctx.focus_target = Some(ctx.node_id);
                        *ctx.handled = true;
                    }
                    ElementState::Released => {
                        let was = self.pressed;
                        self.pressed = false;
                        *ctx.handled = true;
                        if was {
                            self.fire_press();
                        }
                    }
                }
            }
            WindowEvent::KeyboardInput { event, .. }
                if self.focused && event.state == ElementState::Pressed =>
            {
                if matches!(
                    &event.logical_key,
                    Key::Named(NamedKey::Enter | NamedKey::Space)
                ) {
                    self.fire_press();
                    *ctx.handled = true;
                }
            }
            _ => {}
        }
    }

    fn is_focusable(&self) -> bool {
        true
    }

    fn set_focus(&mut self, focused: bool) {
        self.focused = focused;
    }

    fn is_focused(&self) -> bool {
        self.focused
    }

    fn accessibility(&self) -> Option<tilo_ui_base::a11y::AccessNode> {
        use tilo_ui_base::{a11y::AccessNode, a11y::AccessState, a11y::Role};
        let mut node = AccessNode::new(Role::Button)
            .with_name(&self.props.name)
            .with_bounds(self.rect)
            .with_state(
                AccessState::new()
                    .focusable(true)
                    .focused(self.focused)
                    .disabled(!self.props.enabled),
            );
        // Why it cannot be pressed now, if it says: read beside its name, never instead of it.
        if let Some(why) = self
            .props
            .description
            .as_deref()
            .filter(|why| !why.trim().is_empty())
        {
            node = node.with_description(why.to_owned());
        } else if !self.props.who.is_empty() {
            node = node.with_description(
                self.props
                    .who
                    .iter()
                    .map(|who| who.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
            );
        }
        Some(node)
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        (self.props.enabled && self.rect.contains(point))
            .then_some(winit::window::CursorIcon::Pointer)
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn name(&self) -> &'static str {
        "BarChip"
    }
}

impl Described for BarChip {
    type Props = BarChipProps;

    fn from_props(_key: &str, mut props: BarChipProps) -> Self {
        let press = std::mem::replace(&mut props.press, Event::None);
        let press = press.handler();
        Self {
            icon_run: props.icon.into_iter().collect(),
            props,
            press,
            interaction: tilo_ui_base::interaction::InteractionEnvironment::default(),
            rect: Rect::ZERO,
            hovered: false,
            pressed: false,
            focused: false,
            layout_dirty: true,
        }
    }

    fn set_props(&mut self, mut props: BarChipProps) -> bool {
        let press = std::mem::replace(&mut props.press, Event::None);
        self.press = press.handler();
        self.icon_run = props.icon.into_iter().collect();
        self.props = props;
        self.layout_dirty = true;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    tilo_test_env::scenario! {
        fn a_bar_chip_component_describes_and_carries_its_press;

        phase a_bar_chip_describes_its_faces_label_and_press;
        {
            let description = BarChipProps::describe();
            assert_eq!(description.name, "BarChip");
            assert!(description.prop("name").is_some());
            assert!(description.prop("who").is_some());
            assert!(description.event("press").is_some());
        }

        phase a_chip_carries_its_press_into_who_it_names;
        {
            let queue = tilo_ui_base::Intents::default();
            let press = Event::View(tilo_ui_base::component::Intent::new(&queue, "chip", "press"));
            let props = BarChipProps {
                name: "Who is here".into(),
                press,
                ..BarChipProps::default()
            };
            let chip = BarChip::from_props("chip", props);
            assert_eq!(chip.props.name, "Who is here");
            assert!(!chip.is_focused());
        }
    }
}

#[cfg(test)]
mod touch_tests {
    use super::*;

    #[test]
    fn a_coarse_pointer_grows_the_face_to_the_minimum_target() {
        let theme = Theme::dark();
        let mut chip = BarChip::from_props(
            "chip",
            BarChipProps {
                name: "Who is here".into(),
                ..BarChipProps::default()
            },
        );
        assert!((chip.face_height(&theme) - BarChipFace::height(&theme)).abs() < 1e-6);
        chip.set_interaction_environment(tilo_ui_base::interaction::InteractionEnvironment::TOUCH);
        assert!(
            chip.face_height(&theme) >= 48.0 - 1e-6,
            "a finger's target grows the face"
        );
    }
}
