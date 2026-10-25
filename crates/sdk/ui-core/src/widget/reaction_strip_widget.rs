//! [`ReactionStripWidget`]: the reaction strip as a retained widget.

use std::collections::HashMap;
use std::rc::Rc;

use taffy::{NodeId, Style, TaffyTree};
use tilo_resource::{ReactionGroup, Reactor};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use super::{ReactionStrip, StripHit, StripMetrics, layout};
use crate::paint::PaintCtx;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::{
    ActionEmitter, ControlBindingKind, ControlBindingMetadata, EventCtx, ModelPath, TaffyTreeExt,
    Widget,
};

/// A person reacting with an emoji (`react`), or taking their reaction
/// back: what a pill or a pick in the picker asks for.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ReactionToggle {
    pub emoji: String,
    pub react: bool,
}

/// The reactions to one thing, as a widget: pills that toggle your own
/// reaction, and an add button that asks for another emoji.
///
/// A toggle goes to the bound action ([`Self::on_action`]; a framework view
/// binds it to the field's `react`/`unreact` edits) or to
/// [`Self::on_toggle`], and is also kept for [`Self::take_toggles`]. The add
/// button's press is kept for [`Self::take_add_request`] with where it is,
/// for the host to open the emoji picker there.
pub struct ReactionStripWidget {
    groups: Vec<ReactionGroup>,
    me: String,
    can_react: bool,
    names: Rc<dyn Fn(&Reactor) -> String>,
    strip: ReactionStrip,
    metrics: Option<StripMetrics>,
    rect: Rect,
    hover: Option<Point>,
    focused: bool,
    /// The control the keyboard is on, among the strip's targets.
    focus_index: usize,
    toggles: Vec<ReactionToggle>,
    add_request: Option<Rect>,
    on_toggle: Option<Box<dyn FnMut(ReactionToggle)>>,
    /// The emoji picker the add button opens, unless the host opens its own.
    picker: Option<crate::widget::Popover<crate::widget::EmojiPicker>>,
    action: Option<ActionEmitter<ReactionToggle>>,
    test_id: Option<String>,
    dirty: bool,
    layout_dirty: bool,
}

impl ReactionStripWidget {
    /// The reactions `groups` (from [`tilo_resource::Reactions::groups`]),
    /// seen by the participant `me`.
    #[must_use]
    pub fn new(groups: Vec<ReactionGroup>, me: impl Into<String>) -> Self {
        Self {
            groups,
            me: me.into(),
            can_react: true,
            names: Rc::new(|reactor: &Reactor| reactor.by.clone()),
            strip: ReactionStrip::new(),
            metrics: None,
            rect: Rect::ZERO,
            hover: None,
            focused: false,
            focus_index: 0,
            toggles: Vec::new(),
            add_request: None,
            on_toggle: None,
            picker: Some(
                crate::widget::Popover::with_content(crate::widget::EmojiPicker::new())
                    .with_label(crate::i18n::widget_format("emoji-chooser-title", &[])),
            ),
            action: None,
            test_id: None,
            dirty: true,
            layout_dirty: true,
        }
    }

    /// How a reactor is named in a pill's tooltip (a participant's display
    /// name); by default their id.
    #[must_use]
    pub fn with_names(mut self, names: impl Fn(&Reactor) -> String + 'static) -> Self {
        self.names = Rc::new(names);
        self
    }

    /// Whether the person may react: without it the pills only show, and
    /// there is no add button.
    #[must_use]
    pub fn can_react(mut self, can: bool) -> Self {
        self.can_react = can;
        self.layout_dirty = true;
        self
    }

    /// Tell `toggle` each toggle.
    #[must_use]
    pub fn on_toggle(mut self, toggle: impl FnMut(ReactionToggle) + 'static) -> Self {
        self.on_toggle = Some(Box::new(toggle));
        self
    }

    /// Invoke `binding` with each toggle.
    #[must_use]
    pub fn on_action(mut self, binding: tilo_actions::ActionBinding<ReactionToggle>) -> Self {
        self.action = Some(ActionEmitter::new(binding));
        self
    }

    /// Leave the add button's picker to the host, which opens its own at
    /// [`Self::take_add_request`].
    #[must_use]
    pub fn without_picker(mut self) -> Self {
        self.picker = None;
        self
    }

    /// Whether the add button's picker is open.
    #[must_use]
    pub fn is_picking(&self) -> bool {
        self.picker
            .as_ref()
            .is_some_and(crate::widget::Popover::is_open)
    }

    #[must_use]
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.test_id = Some(id.into());
        self
    }

    /// Show `groups` now: a new pill arrives, a changed count cross-fades.
    pub fn set_reactions(&mut self, groups: Vec<ReactionGroup>) {
        if groups != self.groups {
            self.groups = groups;
            self.dirty = true;
            self.layout_dirty = true;
        }
    }

    /// The strip as last laid out and drawn.
    #[must_use]
    pub fn strip(&self) -> &ReactionStrip {
        &self.strip
    }

    /// The toggles asked for since the last call, oldest first.
    pub fn take_toggles(&mut self) -> Vec<ReactionToggle> {
        std::mem::take(&mut self.toggles)
    }

    /// Where the add button was pressed, since the last call.
    pub fn take_add_request(&mut self) -> Option<Rect> {
        self.add_request.take()
    }

    /// React with `emoji`, or take your reaction back when you reacted with
    /// it: what a pill does, and what the host does with a pick from the
    /// picker.
    pub fn toggle(&mut self, emoji: &str) {
        if !self.can_react {
            return;
        }
        let react = !self
            .groups
            .iter()
            .any(|group| group.emoji == emoji && group.includes(&self.me));
        let toggle = ReactionToggle {
            emoji: emoji.to_owned(),
            react,
        };
        if react {
            crate::emoji_catalogue::record_recent(emoji);
        }
        if let Some(action) = &mut self.action
            && let Err(error) = action.emit(toggle.clone())
        {
            tracing::warn!(%error, "the reaction's action refused it");
        }
        if let Some(on_toggle) = &mut self.on_toggle {
            on_toggle(toggle.clone());
        }
        self.toggles.push(toggle);
        self.dirty = true;
    }

    fn activate(&mut self, hit: StripHit) {
        match hit {
            StripHit::Toggle(emoji) => self.toggle(&emoji),
            StripHit::Add { anchor } => {
                self.add_request = Some(anchor);
                if let Some(picker) = &mut self.picker {
                    picker.set_anchor(anchor);
                    picker.content_mut().reopen();
                    picker.set_open(true);
                }
            }
        }
        self.dirty = true;
    }

    fn relayout(&mut self, text: &mut TextRenderer, theme: &Theme) {
        let metrics = StripMetrics::of(theme);
        let laid = layout(
            &self.groups,
            &self.me,
            self.can_react,
            (self.rect.origin(), self.rect.width()),
            text,
            &metrics,
        );
        self.strip.set_layout(laid, theme.motion());
        self.metrics = Some(metrics);
    }

    fn focused_hit(&self) -> Option<StripHit> {
        if !self.focused {
            return None;
        }
        self.strip
            .laid_out()
            .targets()
            .into_iter()
            .nth(self.focus_index)
            .map(|(_, hit)| hit)
    }
}

impl Widget for ReactionStripWidget {
    crate::provides!(dyn crate::widget::controls::ActionControls);

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.metrics = Some(StripMetrics::of(theme));
        taffy.widget_leaf(Style::default(), node_map, self)
    }

    /// As wide as its pills within the room offered, as tall as the rows
    /// they wrap onto.
    fn measure(
        &mut self,
        known: taffy::Size<Option<f32>>,
        available: taffy::Size<taffy::AvailableSpace>,
        text: &mut TextRenderer,
    ) -> taffy::Size<f32> {
        let Some(metrics) = self.metrics.clone() else {
            return taffy::Size::ZERO;
        };
        let width = known.width.map_or_else(
            || match available.width {
                taffy::AvailableSpace::Definite(width) => f64::from(width),
                _ => f64::INFINITY,
            },
            f64::from,
        );
        let laid = layout(
            &self.groups,
            &self.me,
            self.can_react,
            (Point::ZERO, width),
            text,
            &metrics,
        );
        let bounds = laid.bounds();
        taffy::Size {
            width: crate::num::f32_of_f64(bounds.width()),
            height: crate::num::f32_of_f64(bounds.height()),
        }
    }

    fn is_layout_dirty(&self) -> bool {
        self.layout_dirty
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
        self.layout_dirty = false;
        self.dirty = true;
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        self.relayout(text, theme);
        let focus = self.focused_hit();
        self.strip.paint(
            &mut PaintCtx::new(scene, transform, text, theme),
            self.hover,
            focus.as_ref(),
        );
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
            || self.strip.is_moving()
            || self.picker.as_ref().is_some_and(Widget::needs_redraw)
    }

    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if let Some(picker) = &mut self.picker {
            picker.draw_overlay(scene, transform, text, theme, presence);
        }
    }

    fn modal_capture(&self) -> Option<Rect> {
        self.picker.as_ref().and_then(Widget::modal_capture)
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
        if let Some(picker) = &mut self.picker {
            picker.mark_drawn();
        }
    }

    fn needs_animation_frame(&self) -> bool {
        self.strip.is_moving()
            || self
                .picker
                .as_ref()
                .is_some_and(Widget::needs_animation_frame)
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        if let Some(picker) = self.picker.as_mut().filter(|picker| picker.is_open()) {
            picker.handle_event(event, ctx);
            let mut picked = None;
            for event in picker.content_mut().take_events() {
                if let crate::widget::PickerEvent::Picked(emoji) = event {
                    picked = Some(emoji);
                }
                picker.close();
            }
            if !picker.content().is_open() {
                picker.close();
            }
            if let Some(emoji) = picked {
                self.toggle(&emoji);
            }
            self.dirty = true;
            return;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let point =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                let hover = self.rect.contains(point).then_some(point);
                if hover != self.hover {
                    let was = self.hover.and_then(|p| self.strip.hit(p));
                    self.hover = hover;
                    if hover.and_then(|p| self.strip.hit(p)) != was {
                        self.dirty = true;
                    }
                }
            }
            WindowEvent::CursorLeft { .. } => {
                if self.hover.take().is_some() {
                    self.dirty = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if let Some(hit) = self.hover.and_then(|p| self.strip.hit(p)) {
                    if let Some(index) = self
                        .strip
                        .laid_out()
                        .targets()
                        .iter()
                        .position(|(_, target)| *target == hit)
                    {
                        self.focus_index = index;
                    }
                    self.activate(hit);
                    *ctx.focus_target = Some(ctx.node_id);
                    *ctx.handled = true;
                }
            }
            WindowEvent::KeyboardInput { event, .. }
                if self.focused && event.state == ElementState::Pressed =>
            {
                let count = self.strip.laid_out().targets().len();
                match &event.logical_key {
                    Key::Named(NamedKey::ArrowLeft) if self.focus_index > 0 => {
                        self.focus_index -= 1;
                    }
                    Key::Named(NamedKey::ArrowRight) if self.focus_index + 1 < count => {
                        self.focus_index += 1;
                    }
                    Key::Named(NamedKey::Home) => self.focus_index = 0,
                    Key::Named(NamedKey::End) => self.focus_index = count.saturating_sub(1),
                    Key::Named(NamedKey::Enter | NamedKey::Space) => {
                        if let Some(hit) = self.focused_hit() {
                            self.activate(hit);
                        }
                    }
                    _ => return,
                }
                self.dirty = true;
                *ctx.handled = true;
            }
            _ => {}
        }
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        self.strip
            .hit(point)
            .map(|_| winit::window::CursorIcon::Pointer)
    }

    fn tooltip_target_at(&self, point: Point) -> Option<(Rect, String)> {
        let names = Rc::clone(&self.names);
        self.strip.tooltip_at(point, &move |reactor| names(reactor))
    }

    fn is_focusable(&self) -> bool {
        self.can_react
    }

    fn set_focus(&mut self, focused: bool) {
        if self.focused != focused {
            self.focused = focused;
            self.dirty = true;
        }
    }

    fn is_focused(&self) -> bool {
        self.focused
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        let mut children = self.strip.accessibility();
        if let Some(picker) = self.picker.as_ref().filter(|picker| picker.is_open()) {
            children.extend(picker.content().accessibility());
        }
        Some(
            crate::a11y::AccessNode::new(crate::a11y::Role::Group)
                .with_bounds(self.rect)
                .with_children(children),
        )
    }

    fn perform_action(
        &mut self,
        action: &crate::a11y::Action,
        key_path: &[u64],
        _state: &mut crate::widget::Edits<'_>,
        _path: &ModelPath,
    ) -> bool {
        if !matches!(
            action,
            crate::a11y::Action::Activate | crate::a11y::Action::ToggleCheck
        ) {
            return false;
        }
        let Some(key) = key_path.first() else {
            return false;
        };
        let hit = self
            .strip
            .laid_out()
            .targets()
            .into_iter()
            .map(|(_, hit)| hit)
            .find(|hit| match hit {
                StripHit::Toggle(emoji) => crate::a11y::hash_key(&("reaction", emoji)) == *key,
                StripHit::Add { .. } => crate::a11y::hash_key(&"reaction-add") == *key,
            });
        match hit {
            Some(hit) => {
                self.activate(hit);
                true
            }
            None => false,
        }
    }

    fn name(&self) -> &'static str {
        "ReactionStrip"
    }

    fn test_id(&self) -> Option<&str> {
        self.test_id.as_deref()
    }
}

impl crate::widget::controls::ActionControls for ReactionStripWidget {
    crate::widget::controls::optional_emitter_controls!();
    fn control_action_bindings(&self) -> Vec<ControlBindingMetadata> {
        let control_id = self.test_id().unwrap_or("reaction_strip");
        if let Some(action) = &self.action {
            vec![action.metadata(control_id)]
        } else {
            vec![ControlBindingMetadata {
                control_id: control_id.to_string(),
                kind: ControlBindingKind::SemanticAction(control_id.to_string()),
            }]
        }
    }
}
